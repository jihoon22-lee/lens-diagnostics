//! # Lens Disk (`lens-disk`)
//!
//! High-performance filesystem diagnostics, storage tree analysis, and safe cleanup workbench.
//!
//! Re-architects and replaces legacy `diskmap` with:
//! - **Arena Tree ([`arena::ArenaTree`])**: Contiguous `u32`-indexed node storage
//!   with O(1) child insertion and iterative aggregation.
//! - **Scanner ([`scanner::DiskScanner`])**: Bounded traversal with mount and cycle
//!   safeguards; optional rayon-parallel stat.
//! - **Duplicate Finder ([`duplicates::DuplicateFinder`])**: Multi-stage (size -> inode -> 4KB partial -> full SHA-256) with hardlink deduplication.
//! - **FreeDesktop Trash Manager ([`trash::TrashManager`])**: XDG spec-compliant trash movement and audit receipt restoration.
//! - **Snapshot V2 ([`snapshot::SnapshotV2`])**: 100% compliant with `diskmap.snapshot/v2` schema.

pub mod arena;
pub mod duplicates;
pub mod scanner;
pub mod snapshot;
pub mod trash;

pub use arena::{ArenaTree, DiskMetadata, FsKind, FsNode, NodeId};
pub use duplicates::{DuplicateFinder, DuplicateGroup, DuplicateReport};
pub use scanner::{DiskScanner, ScanOptions, ScanResult};
pub use snapshot::{SnapshotDiff, SnapshotNodeV2, SnapshotV2, SCHEMA_V2};
pub use trash::TrashEntry;
pub use trash::{TrashManager, TrashReceipt};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scan_and_snapshot_roundtrip() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file_a.txt"), b"12345678").unwrap();
        fs::write(sub.join("file_b.txt"), b"abcdef").unwrap();

        let scanner = DiskScanner::new(ScanOptions::default());
        let res = scanner.scan(dir.path()).unwrap();

        assert_eq!(res.scanned_entries, 4); // root, subdir, 2 files
        assert!(res.complete);

        let snap = SnapshotV2::from_tree(&res.tree, res.root_id, res.complete, res.truncated);
        assert_eq!(snap.schema_version, "diskmap.snapshot/v2");
        assert_eq!(snap.root.size, 14); // 8 + 6

        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("diskmap.snapshot/v2"));
    }

    #[test]
    fn hardlink_dedup_across_different_directories() {
        // a/f and b/g are two names for one 5-byte inode; c/h is a
        // separate 3-byte file. The root aggregate must count the
        // shared inode once: 5 + 3 = 8, and the directory holding the
        // later occurrence must aggregate 0 for it.
        let dir = tempdir().unwrap();
        for sub in ["a", "b", "c"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
        }
        fs::write(dir.path().join("a/f"), b"12345").unwrap();
        if fs::hard_link(dir.path().join("a/f"), dir.path().join("b/g")).is_err() {
            return; // filesystem without hardlink support
        }
        fs::write(dir.path().join("c/h"), b"xyz").unwrap();

        let res = DiskScanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let root = &res.tree.nodes[res.root_id as usize];
        assert_eq!(root.size, 8, "hardlink counted once + 3-byte file");

        // Whichever subdir holds the *later* occurrence contributes 0;
        // the other contributes 5. Find each subdir's aggregate.
        let mut a_size = None;
        let mut b_size = None;
        let mut c_size = None;
        for child in res.tree.children_ids(res.root_id) {
            let node = &res.tree.nodes[child as usize];
            match node.name.as_str() {
                "a" => a_size = Some(node.size),
                "b" => b_size = Some(node.size),
                "c" => c_size = Some(node.size),
                _ => {}
            }
        }
        let (a_size, b_size, c_size) = (a_size.unwrap(), b_size.unwrap(), c_size.unwrap());
        assert_eq!(a_size + b_size, 5, "shared inode counted once, not in both");
        assert_eq!(c_size, 3);
        // Both file nodes keep their own size.
        let file_sizes: Vec<u64> = res
            .tree
            .children_ids(res.root_id)
            .iter()
            .flat_map(|d| res.tree.children_ids(*d))
            .map(|f| res.tree.nodes[f as usize].size)
            .collect();
        assert_eq!(file_sizes, vec![5, 5, 3]);
    }

    #[test]
    fn test_duplicate_detection() {
        let dir = tempdir().unwrap();
        let f1 = dir.path().join("copy1.bin");
        let f2 = dir.path().join("copy2.bin");
        let f3 = dir.path().join("diff.bin");

        let content = vec![0x42u8; 8192];
        fs::write(&f1, &content).unwrap();
        fs::write(&f2, &content).unwrap();
        fs::write(&f3, b"short content").unwrap();

        let scanner = DiskScanner::new(ScanOptions::default());
        let res = scanner.scan(dir.path()).unwrap();

        let finder = DuplicateFinder::new(1);
        let report = finder.find_in_tree(&res.tree, dir.path()).unwrap();
        let groups = &report.groups;

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size, 8192);
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].reclaimable_bytes, 8192);
        assert_eq!(report.total_reclaimable_bytes, 8192);
        assert!(report.errors.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_duplicate_hash_failures_reported_not_dropped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let content = vec![0x42u8; 8192];
        fs::write(dir.path().join("a.bin"), &content).unwrap();
        fs::write(dir.path().join("b.bin"), &content).unwrap();
        let locked = dir.path().join("c.bin");
        fs::write(&locked, &content).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let scanner = DiskScanner::new(ScanOptions::default());
        let res = scanner.scan(dir.path()).unwrap();
        let report = DuplicateFinder::new(1)
            .find_in_tree(&res.tree, dir.path())
            .unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

        if report.errors.is_empty() {
            // The filesystem honored reads anyway (e.g. root) — nothing to
            // assert about the failure path.
            return;
        }
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("c.bin"));
        // The two readable copies still form a group.
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].files.len(), 2);
    }

    #[test]
    fn test_trash_and_restore() {
        let dir = tempdir().unwrap();
        let target_file = dir.path().join("doomed.txt");
        fs::write(&target_file, b"important data").unwrap();

        let trash_dir = dir.path().join("custom_trash");
        let trash = TrashManager::new(trash_dir);

        let receipt = trash.move_to_trash(&target_file).unwrap();
        assert!(!target_file.exists());
        assert!(receipt.trashed_file_path.exists());
        assert!(receipt.info_path.exists());

        // Restore
        trash.restore(&receipt).unwrap();
        assert!(target_file.exists());
        assert_eq!(fs::read(&target_file).unwrap(), b"important data");
    }

    #[test]
    fn test_trash_moves_symlink_not_target() {
        let dir = tempdir().unwrap();
        let target_file = dir.path().join("evidence.txt");
        fs::write(&target_file, b"keep me").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target_file, &link).unwrap();

        let trash = TrashManager::new(dir.path().join("trash"));
        let receipt = trash.move_to_trash(&link).unwrap();

        // The link was trashed; the target must be untouched.
        assert!(target_file.exists());
        assert!(link.symlink_metadata().is_err());
        assert!(receipt
            .trashed_file_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn test_symlink_alias_does_not_hide_real_dir() {
        // A symlink sorting earlier (`0link`) must not mark the real dir's
        // inode visited and hide its subtree from the scan.
        let dir = tempdir().unwrap();
        let real = dir.path().join("zreal");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("secret.txt"), b"evidence").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("0link")).unwrap();

        let scanner = DiskScanner::new(ScanOptions::default());
        let res = scanner.scan(dir.path()).unwrap();

        assert_eq!(res.scanned_entries, 4); // root, link, real dir, secret.txt
        let names: Vec<String> = res.tree.nodes.iter().map(|n| n.name.clone()).collect();
        assert!(names.contains(&"secret.txt".to_string()));
        assert_eq!(names.iter().filter(|n| *n == "zreal").count(), 1);
    }

    #[test]
    fn test_parallel_scan_matches_serial() {
        let dir = tempdir().unwrap();
        for i in 0..8 {
            let sub = dir.path().join(format!("d{}", i));
            fs::create_dir(&sub).unwrap();
            for j in 0..4 {
                fs::write(sub.join(format!("f{}.bin", j)), vec![i as u8; 64]).unwrap();
            }
        }

        let serial = DiskScanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let parallel = DiskScanner::new(ScanOptions::default())
            .with_parallel(true)
            .scan(dir.path())
            .unwrap();

        assert_eq!(serial.scanned_entries, parallel.scanned_entries);
        assert_eq!(serial.complete, parallel.complete);
        assert_eq!(serial.errors, parallel.errors);
        let serial_names: Vec<String> = serial.tree.nodes.iter().map(|n| n.name.clone()).collect();
        let parallel_names: Vec<String> =
            parallel.tree.nodes.iter().map(|n| n.name.clone()).collect();
        assert_eq!(serial_names, parallel_names);
    }

    #[test]
    fn test_depth_cap_marks_scan_incomplete() {
        let dir = tempdir().unwrap();
        let mut deep = dir.path().join("a");
        fs::create_dir(&deep).unwrap();
        for c in ["b", "c"] {
            deep = deep.join(c);
            fs::create_dir(&deep).unwrap();
        }
        fs::write(deep.join("hidden.txt"), b"x").unwrap();

        let opts = ScanOptions {
            max_depth: 2,
            ..Default::default()
        };
        let res = DiskScanner::new(opts).scan(dir.path()).unwrap();

        assert!(!res.complete);
        assert!(res.errors.iter().any(|e| e.contains("Depth limit")));
    }

    #[test]
    fn test_d01_modified_ns_includes_epoch_seconds() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("recent.txt");
        fs::write(&file, b"hello").unwrap();

        let res = DiskScanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let node = res
            .tree
            .nodes
            .iter()
            .find(|n| n.name == "recent.txt")
            .unwrap();
        assert!(
            node.metadata.modified_ns >= 1_700_000_000_000_000_000,
            "modified_ns was {}",
            node.metadata.modified_ns
        );
    }

    #[test]
    fn test_d03_deep_snapshot_serde_roundtrip() {
        // Build a 100-level deep tree and verify SnapshotV2 serializes and
        // deserializes via serde_json without hitting the 128 recursion limit.
        let mut tree = ArenaTree::new();
        let root_id = tree.alloc(FsNode::new("root".to_string(), ".".to_string(), true));
        let mut parent = root_id;
        for depth in 1..=100 {
            let is_dir = depth < 100;
            let name = format!("lvl_{depth}");
            let rel_path = format!("p/{depth}");
            let mut node = FsNode::new(name, rel_path, is_dir);
            if !is_dir {
                node.size = 42;
                node.metadata.kind = FsKind::RegularFile;
            } else {
                node.metadata.kind = FsKind::Directory;
            }
            let child_id = tree.alloc(node);
            tree.add_child(parent, child_id);
            parent = child_id;
        }
        tree.aggregate_sizes(root_id);

        let snap = SnapshotV2::from_tree(&tree, root_id, true, false);
        assert_eq!(snap.root.size, 42);

        let json = serde_json::to_string(&snap).expect("serialize 100-level snapshot");
        let decoded: SnapshotV2 =
            serde_json::from_str(&json).expect("deserialize 100-level snapshot");
        assert_eq!(snap, decoded);

        let diff = SnapshotDiff::compute(&snap, &decoded);
        assert_eq!(diff.net_bytes_delta, 0);
        assert!(diff.added.is_empty() && diff.removed.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_d04_duplicates_skips_fifo_and_reuses_small_partial_hash() {
        let dir = tempdir().unwrap();
        let fifo_path = dir.path().join("pipe.fifo");
        let c_path = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0);

        let f1 = dir.path().join("small1.txt");
        let f2 = dir.path().join("small2.txt");
        fs::write(&f1, b"small duplicate payload").unwrap();
        fs::write(&f2, b"small duplicate payload").unwrap();

        // Hardlink pair of unique size: should be skipped before partial hashing.
        let hl1 = dir.path().join("hl1.txt");
        let hl2 = dir.path().join("hl2.txt");
        fs::write(&hl1, b"unique-hardlink-size-content").unwrap();
        let _ = fs::hard_link(&hl1, &hl2);

        use std::os::unix::ffi::OsStrExt;
        let res = DiskScanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        // min_size = 0 must not open the 0-byte FIFO and block.
        let report = DuplicateFinder::new(0)
            .find_in_tree(&res.tree, dir.path())
            .unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].files.len(), 2);
    }

    #[test]
    fn test_d05_symlink_is_not_dir_alphabetical_visit_and_max_entries_truncation() {
        let dir = tempdir().unwrap();
        let sub_a = dir.path().join("a_dir");
        let sub_b = dir.path().join("b_dir");
        fs::create_dir(&sub_a).unwrap();
        fs::create_dir(&sub_b).unwrap();
        fs::write(sub_a.join("inside_a.txt"), b"aaa").unwrap();
        fs::write(sub_b.join("inside_b.txt"), b"bbb").unwrap();
        std::os::unix::fs::symlink(&sub_a, dir.path().join("link_to_dir")).unwrap();

        let res = DiskScanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let link_node = res
            .tree
            .nodes
            .iter()
            .find(|n| n.name == "link_to_dir")
            .unwrap();
        assert!(!link_node.is_dir, "symlink to dir must have is_dir = false");
        assert_eq!(link_node.metadata.kind, FsKind::Symlink);

        // Alphabetical visit order: a_dir's child is allocated before b_dir's child.
        let pos_a = res
            .tree
            .nodes
            .iter()
            .position(|n| n.name == "inside_a.txt")
            .unwrap();
        let pos_b = res
            .tree
            .nodes
            .iter()
            .position(|n| n.name == "inside_b.txt")
            .unwrap();
        assert!(pos_a < pos_b, "expected inside_a before inside_b");

        // Truncated scan marks root and unvisited directories incomplete.
        let trunc_res = DiskScanner::new(ScanOptions {
            max_entries: 3,
            ..Default::default()
        })
        .scan(dir.path())
        .unwrap();
        assert!(trunc_res.truncated);
        assert!(!trunc_res.complete);
        assert!(!trunc_res.tree.nodes[trunc_res.root_id as usize].complete);
    }
}
