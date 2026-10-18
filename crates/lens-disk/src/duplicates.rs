use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::arena::ArenaTree;
use lens_core::{digest_bytes, digest_file, Result};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub size: u64,
    pub sha256: String,
    pub files: Vec<PathBuf>,
    pub reclaimable_bytes: u64,
}

/// Duplicate groups plus evidence gaps — hash failures are reported
/// instead of silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub errors: Vec<String>,
    /// Sum of `reclaimable_bytes` across all groups.
    pub total_reclaimable_bytes: u64,
}

const PARTIAL_HASH_BYTES: u64 = 4096;

pub struct DuplicateFinder {
    min_size: u64,
}

impl DuplicateFinder {
    pub fn new(min_size: u64) -> Self {
        Self { min_size }
    }

    /// Finds duplicates across files registered in an ArenaTree.
    pub fn find_in_tree(&self, tree: &ArenaTree, base_dir: &Path) -> Result<DuplicateReport> {
        // Step 1: Bucket regular files by size. Non-regular files (symlinks,
        // FIFOs, sockets, device nodes) are excluded so we never block on pipes
        // or compare symlink target bytes. Hardlinks are folded by (dev, ino)
        // using the identity already captured during the scan — no extra stat,
        // and each inode is hashed at most once.
        use crate::arena::FsKind;

        // Pass 1: count regular files by size so only multi-candidate sizes pay
        // the PathBuf materialization cost.
        let mut file_count_by_size: HashMap<u64, usize> = HashMap::new();
        for node in &tree.nodes {
            if node.is_dir || node.metadata.kind != FsKind::RegularFile || node.size < self.min_size
            {
                continue;
            }
            *file_count_by_size.entry(node.size).or_default() += 1;
        }

        // Pass 2: bucket by size → inode. Filesystems reporting inode 0 get
        // keyed by path so they fold into singleton buckets instead of one
        // giant bucket that masks every duplicate.
        let mut size_buckets: HashMap<u64, HashMap<String, Vec<PathBuf>>> = HashMap::new();
        for node in &tree.nodes {
            if node.is_dir || node.metadata.kind != FsKind::RegularFile || node.size < self.min_size
            {
                continue;
            }
            if file_count_by_size.get(&node.size).copied().unwrap_or(0) < 2 {
                continue;
            }
            let id = node.metadata.identity;
            let key = if id.device == 0 && id.inode == 0 {
                format!("p:{}", base_dir.join(&node.rel_path).display())
            } else {
                format!("i:{}:{}", id.device, id.inode)
            };
            size_buckets
                .entry(node.size)
                .or_default()
                .entry(key)
                .or_default()
                .push(base_dir.join(&node.rel_path));
        }

        let mut groups = Vec::new();
        let mut errors = Vec::new();

        for (size, inode_buckets) in size_buckets {
            // Skip size buckets where all files are hardlinks to a single inode.
            if inode_buckets.len() < 2 {
                continue;
            }

            // Step 2: Partial hash (first 4KB) once per unique inode.
            let mut partial_buckets: HashMap<String, Vec<String>> = HashMap::new();
            for (key, paths) in &inode_buckets {
                match Self::compute_partial_hash(&paths[0]) {
                    Ok(partial_hash) => partial_buckets
                        .entry(partial_hash)
                        .or_default()
                        .push(key.clone()),
                    Err(e) => errors.push(format!("{}: {}", paths[0].display(), e)),
                }
            }

            // Step 3: Full SHA-256 for matching partial hashes.
            for (p_hash, inode_ids) in partial_buckets {
                if inode_ids.len() < 2 {
                    continue;
                }

                let mut full_buckets: HashMap<String, Vec<String>> = HashMap::new();
                if size <= PARTIAL_HASH_BYTES {
                    // For files <= 4 KiB, the partial hash already covered the entire file.
                    full_buckets.insert(p_hash, inode_ids);
                } else {
                    for id in inode_ids {
                        match digest_file(&inode_buckets[&id][0]) {
                            Ok(full_hash) => full_buckets.entry(full_hash).or_default().push(id),
                            Err(e) => {
                                errors.push(format!("{}: {}", inode_buckets[&id][0].display(), e))
                            }
                        }
                    }
                }

                for (sha256, ids) in full_buckets {
                    if ids.len() < 2 {
                        continue;
                    }
                    let mut files: Vec<PathBuf> = ids
                        .iter()
                        .flat_map(|id| inode_buckets[id].iter().cloned())
                        .collect();
                    files.sort();
                    let distinct_copies = ids.len();
                    let reclaimable_bytes = if distinct_copies > 1 {
                        (distinct_copies as u64 - 1) * size
                    } else {
                        0
                    };

                    groups.push(DuplicateGroup {
                        size,
                        sha256,
                        files,
                        reclaimable_bytes,
                    });
                }
            }
        }

        // Sort descending by reclaimable bytes, then sha for determinism.
        groups.sort_by(|a, b| {
            b.reclaimable_bytes
                .cmp(&a.reclaimable_bytes)
                .then_with(|| a.sha256.cmp(&b.sha256))
        });
        errors.sort();
        errors.dedup();
        let total_reclaimable_bytes = groups.iter().map(|g| g.reclaimable_bytes).sum();

        Ok(DuplicateReport {
            groups,
            errors,
            total_reclaimable_bytes,
        })
    }

    fn compute_partial_hash(path: &Path) -> std::io::Result<String> {
        // `read` may return short reads on FUSE/NFS; loop until EOF or the
        // 4 KiB window is full so identical files can't land in different
        // buckets.
        let file = File::open(path)?;
        let mut buffer = Vec::with_capacity(PARTIAL_HASH_BYTES as usize);
        file.take(PARTIAL_HASH_BYTES).read_to_end(&mut buffer)?;
        Ok(digest_bytes(&buffer))
    }
}
