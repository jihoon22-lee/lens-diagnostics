use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::Cell;
use std::collections::HashMap;

use crate::arena::{ArenaTree, DiskMetadata, FsKind, NodeId};
use crate::scanner::ScanResult;

pub const SCHEMA_V2: &str = "diskmap.snapshot/v2";

/// Inline nesting threshold before flattening remaining descendants so
/// `serde_json`'s default 128-level recursion limit is never exceeded.
const MAX_INLINE_DEPTH: usize = 32;

thread_local! {
    static SER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct DepthGuard {
    prev: usize,
}

impl DepthGuard {
    fn enter() -> Self {
        let prev = SER_DEPTH.with(|d| {
            let p = d.get();
            d.set(p + 1);
            p
        });
        Self { prev }
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        SER_DEPTH.with(|d| d.set(self.prev));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotIdentityV2 {
    pub device: u64,
    pub file: u64,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMetadataV2 {
    pub allocated_size: u64,
    pub group: u64,
    pub hard_link_count: u64,
    pub hard_link_count_known: bool,
    pub identity: SnapshotIdentityV2,
    pub kind: String,
    pub logical_size: u64,
    pub modified_ns: i64,
    pub modified_time_known: bool,
    pub owner: u64,
    pub ownership_known: bool,
    pub permissions: u32,
    pub permissions_known: bool,
}

impl Default for SnapshotMetadataV2 {
    fn default() -> Self {
        Self {
            allocated_size: 0,
            group: 0,
            hard_link_count: 1,
            hard_link_count_known: true,
            identity: SnapshotIdentityV2 {
                device: 0,
                file: 0,
                valid: false,
            },
            kind: "other".to_string(),
            logical_size: 0,
            modified_ns: 0,
            modified_time_known: false,
            owner: 0,
            ownership_known: false,
            permissions: 0,
            permissions_known: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotNodeV2 {
    pub allocated_size: u64,
    pub allocated_size_known: bool,
    pub children: Vec<SnapshotNodeV2>,
    pub complete: bool,
    pub cycle_skipped: bool,
    pub error: String,
    pub followed: bool,
    pub has_target_metadata: bool,
    pub is_dir: bool,
    pub logical_size_known: bool,
    pub metadata: SnapshotMetadataV2,
    pub mount_boundary_skipped: bool,
    pub name: String,
    pub name_bytes: String,
    pub path: String,
    pub path_bytes: String,
    pub reclaimable_size: u64,
    pub reclaimable_size_known: bool,
    pub size: u64,
    pub target_metadata: SnapshotMetadataV2,
}

impl Drop for SnapshotNodeV2 {
    fn drop(&mut self) {
        let mut stack = std::mem::take(&mut self.children);
        while let Some(mut node) = stack.pop() {
            stack.append(&mut node.children);
        }
    }
}

/// Childless node representation used when flattening deep subtrees past
/// [`MAX_INLINE_DEPTH`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlatSnapshotNodeV2 {
    allocated_size: u64,
    allocated_size_known: bool,
    complete: bool,
    cycle_skipped: bool,
    error: String,
    followed: bool,
    has_target_metadata: bool,
    is_dir: bool,
    logical_size_known: bool,
    metadata: SnapshotMetadataV2,
    mount_boundary_skipped: bool,
    name: String,
    name_bytes: String,
    path: String,
    path_bytes: String,
    reclaimable_size: u64,
    reclaimable_size_known: bool,
    size: u64,
    target_metadata: SnapshotMetadataV2,
}

impl FlatSnapshotNodeV2 {
    fn from_node(n: &SnapshotNodeV2) -> Self {
        Self {
            allocated_size: n.allocated_size,
            allocated_size_known: n.allocated_size_known,
            complete: n.complete,
            cycle_skipped: n.cycle_skipped,
            error: n.error.clone(),
            followed: n.followed,
            has_target_metadata: n.has_target_metadata,
            is_dir: n.is_dir,
            logical_size_known: n.logical_size_known,
            metadata: n.metadata.clone(),
            mount_boundary_skipped: n.mount_boundary_skipped,
            name: n.name.clone(),
            name_bytes: n.name_bytes.clone(),
            path: n.path.clone(),
            path_bytes: n.path_bytes.clone(),
            reclaimable_size: n.reclaimable_size,
            reclaimable_size_known: n.reclaimable_size_known,
            size: n.size,
            target_metadata: n.target_metadata.clone(),
        }
    }

    fn into_node(self, children: Vec<SnapshotNodeV2>) -> SnapshotNodeV2 {
        SnapshotNodeV2 {
            allocated_size: self.allocated_size,
            allocated_size_known: self.allocated_size_known,
            children,
            complete: self.complete,
            cycle_skipped: self.cycle_skipped,
            error: self.error,
            followed: self.followed,
            has_target_metadata: self.has_target_metadata,
            is_dir: self.is_dir,
            logical_size_known: self.logical_size_known,
            metadata: self.metadata,
            mount_boundary_skipped: self.mount_boundary_skipped,
            name: self.name,
            name_bytes: self.name_bytes,
            path: self.path,
            path_bytes: self.path_bytes,
            reclaimable_size: self.reclaimable_size,
            reclaimable_size_known: self.reclaimable_size_known,
            size: self.size,
            target_metadata: self.target_metadata,
        }
    }
}

impl Serialize for SnapshotNodeV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let guard = DepthGuard::enter();
        let flatten = guard.prev >= MAX_INLINE_DEPTH && !self.children.is_empty();
        let field_count = if flatten { 21 } else { 20 };
        let mut map = serializer.serialize_map(Some(field_count))?;
        map.serialize_entry("allocated_size", &self.allocated_size)?;
        map.serialize_entry("allocated_size_known", &self.allocated_size_known)?;
        if flatten {
            let empty: &[SnapshotNodeV2] = &[];
            map.serialize_entry("children", &empty)?;
            let mut flat: Vec<(usize, FlatSnapshotNodeV2)> = Vec::new();
            // Iterative pre-order traversal of descendants: parent index 0 is `self`,
            // parent index k >= 1 is `flat[k - 1]`.
            let mut stack: Vec<(usize, &SnapshotNodeV2)> =
                self.children.iter().rev().map(|c| (0usize, c)).collect();
            while let Some((parent_idx, node)) = stack.pop() {
                flat.push((parent_idx, FlatSnapshotNodeV2::from_node(node)));
                let my_idx = flat.len();
                for child in node.children.iter().rev() {
                    stack.push((my_idx, child));
                }
            }
            map.serialize_entry("flattened_descendants", &flat)?;
        } else {
            map.serialize_entry("children", &self.children)?;
        }
        map.serialize_entry("complete", &self.complete)?;
        map.serialize_entry("cycle_skipped", &self.cycle_skipped)?;
        map.serialize_entry("error", &self.error)?;
        map.serialize_entry("followed", &self.followed)?;
        map.serialize_entry("has_target_metadata", &self.has_target_metadata)?;
        map.serialize_entry("is_dir", &self.is_dir)?;
        map.serialize_entry("logical_size_known", &self.logical_size_known)?;
        map.serialize_entry("metadata", &self.metadata)?;
        map.serialize_entry("mount_boundary_skipped", &self.mount_boundary_skipped)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("name_bytes", &self.name_bytes)?;
        map.serialize_entry("path", &self.path)?;
        map.serialize_entry("path_bytes", &self.path_bytes)?;
        map.serialize_entry("reclaimable_size", &self.reclaimable_size)?;
        map.serialize_entry("reclaimable_size_known", &self.reclaimable_size_known)?;
        map.serialize_entry("size", &self.size)?;
        map.serialize_entry("target_metadata", &self.target_metadata)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for SnapshotNodeV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawSnapshotNodeV2 {
            allocated_size: u64,
            allocated_size_known: bool,
            #[serde(default)]
            children: Vec<SnapshotNodeV2>,
            #[serde(default)]
            flattened_descendants: Vec<(usize, FlatSnapshotNodeV2)>,
            complete: bool,
            cycle_skipped: bool,
            error: String,
            followed: bool,
            has_target_metadata: bool,
            is_dir: bool,
            logical_size_known: bool,
            metadata: SnapshotMetadataV2,
            mount_boundary_skipped: bool,
            name: String,
            name_bytes: String,
            path: String,
            path_bytes: String,
            reclaimable_size: u64,
            reclaimable_size_known: bool,
            size: u64,
            target_metadata: SnapshotMetadataV2,
        }

        let raw = RawSnapshotNodeV2::deserialize(deserializer)?;
        let children = if !raw.flattened_descendants.is_empty() {
            let n = raw.flattened_descendants.len();
            let mut slots: Vec<Vec<SnapshotNodeV2>> = (0..=n).map(|_| Vec::new()).collect();
            for (i, (parent_idx, flat_node)) in
                raw.flattened_descendants.into_iter().enumerate().rev()
            {
                let mut my_children = std::mem::take(&mut slots[i + 1]);
                my_children.reverse();
                let node = flat_node.into_node(my_children);
                if parent_idx <= i {
                    slots[parent_idx].push(node);
                }
            }
            let mut root_children = std::mem::take(&mut slots[0]);
            root_children.reverse();
            root_children
        } else {
            raw.children
        };

        Ok(SnapshotNodeV2 {
            allocated_size: raw.allocated_size,
            allocated_size_known: raw.allocated_size_known,
            children,
            complete: raw.complete,
            cycle_skipped: raw.cycle_skipped,
            error: raw.error,
            followed: raw.followed,
            has_target_metadata: raw.has_target_metadata,
            is_dir: raw.is_dir,
            logical_size_known: raw.logical_size_known,
            metadata: raw.metadata,
            mount_boundary_skipped: raw.mount_boundary_skipped,
            name: raw.name,
            name_bytes: raw.name_bytes,
            path: raw.path,
            path_bytes: raw.path_bytes,
            reclaimable_size: raw.reclaimable_size,
            reclaimable_size_known: raw.reclaimable_size_known,
            size: raw.size,
            target_metadata: raw.target_metadata,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotV2 {
    pub schema_version: String,
    pub complete: bool,
    pub truncated: bool,
    pub node_count: u64,
    pub root: SnapshotNodeV2,
}

impl SnapshotV2 {
    pub fn from_scan(scan: &ScanResult) -> Self {
        Self::from_tree(&scan.tree, scan.root_id, scan.complete, scan.truncated)
    }

    pub fn from_tree(tree: &ArenaTree, root_id: NodeId, complete: bool, truncated: bool) -> Self {
        let root = Self::build_snapshot_node_iterative(tree, root_id);
        Self {
            schema_version: SCHEMA_V2.to_string(),
            complete,
            truncated,
            node_count: tree.len() as u64,
            root,
        }
    }

    fn build_snapshot_node_iterative(tree: &ArenaTree, root_id: NodeId) -> SnapshotNodeV2 {
        let mut built: Vec<Option<SnapshotNodeV2>> = (0..tree.len()).map(|_| None).collect();
        let mut stack: Vec<(NodeId, bool)> = vec![(root_id, false)];

        while let Some((node_id, processed)) = stack.pop() {
            if !processed {
                stack.push((node_id, true));
                for child_id in tree.children_ids(node_id).into_iter().rev() {
                    stack.push((child_id, false));
                }
                continue;
            }

            let node = &tree.nodes[node_id as usize];
            let mut children = Vec::new();
            for child_id in tree.children_ids(node_id) {
                if let Some(child_snap) = built[child_id as usize].take() {
                    children.push(child_snap);
                }
            }
            children.sort_by(|a, b| a.name.cmp(&b.name));

            built[node_id as usize] = Some(SnapshotNodeV2 {
                allocated_size: node.allocated_size,
                allocated_size_known: node.allocated_size_known,
                children,
                complete: node.complete,
                cycle_skipped: node.cycle_skipped,
                error: node.error.clone(),
                followed: node.followed,
                has_target_metadata: node.target_metadata.is_some(),
                is_dir: node.is_dir,
                logical_size_known: node.logical_size_known,
                metadata: Self::convert_metadata(&node.metadata),
                mount_boundary_skipped: node.mount_boundary_skipped,
                name: node.name.clone(),
                name_bytes: hex_encode(node.name.as_bytes()),
                path: node.rel_path.clone(),
                path_bytes: hex_encode(node.rel_path.as_bytes()),
                reclaimable_size: node.reclaimable_size,
                reclaimable_size_known: node.reclaimable_size_known,
                size: node.size,
                target_metadata: node
                    .target_metadata
                    .as_ref()
                    .map(Self::convert_metadata)
                    .unwrap_or_default(),
            });
        }

        built[root_id as usize]
            .take()
            .unwrap_or_else(|| SnapshotNodeV2 {
                allocated_size: 0,
                allocated_size_known: false,
                children: Vec::new(),
                complete: false,
                cycle_skipped: false,
                error: String::new(),
                followed: false,
                has_target_metadata: false,
                is_dir: false,
                logical_size_known: false,
                metadata: SnapshotMetadataV2::default(),
                mount_boundary_skipped: false,
                name: String::new(),
                name_bytes: String::new(),
                path: String::new(),
                path_bytes: String::new(),
                reclaimable_size: 0,
                reclaimable_size_known: false,
                size: 0,
                target_metadata: SnapshotMetadataV2::default(),
            })
    }

    fn convert_metadata(meta: &DiskMetadata) -> SnapshotMetadataV2 {
        let kind_str = match meta.kind {
            FsKind::RegularFile => "regular_file",
            FsKind::Directory => "directory",
            FsKind::Symlink => "symlink",
            FsKind::Other => "other",
        };

        SnapshotMetadataV2 {
            allocated_size: meta.allocated_size,
            group: meta.group,
            hard_link_count: meta.hard_link_count,
            hard_link_count_known: meta.hard_link_count_known,
            identity: SnapshotIdentityV2 {
                device: meta.identity.device,
                file: meta.identity.inode,
                valid: meta.identity.inode > 0,
            },
            kind: kind_str.to_string(),
            logical_size: meta.logical_size,
            modified_ns: meta.modified_ns,
            modified_time_known: meta.modified_time_known,
            owner: meta.owner,
            ownership_known: meta.ownership_known,
            permissions: meta.permissions,
            permissions_known: meta.permissions_known,
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub baseline_root: String,
    pub candidate_root: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub grown: Vec<String>,
    pub shrunk: Vec<String>,
    pub net_bytes_delta: i64,
}

impl SnapshotDiff {
    pub fn compute(left: &SnapshotV2, right: &SnapshotV2) -> Self {
        let mut left_map = HashMap::new();
        Self::collect_files(&left.root, &mut left_map);

        let mut right_map = HashMap::new();
        Self::collect_files(&right.root, &mut right_map);

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut grown = Vec::new();
        let mut shrunk = Vec::new();

        for (path, &right_size) in &right_map {
            match left_map.get(path) {
                None => added.push(format!("+ {} ({} B)", path, right_size)),
                Some(&left_size) => {
                    if right_size > left_size {
                        grown.push(format!("^ {} (+{} B)", path, right_size - left_size));
                    } else if right_size < left_size {
                        shrunk.push(format!("v {} (-{} B)", path, left_size - right_size));
                    }
                }
            }
        }

        for (path, &left_size) in &left_map {
            if !right_map.contains_key(path) {
                removed.push(format!("- {} ({} B)", path, left_size));
            }
        }

        added.sort();
        removed.sort();
        grown.sort();
        shrunk.sort();

        let net_bytes_delta = right.root.size as i64 - left.root.size as i64;

        Self {
            baseline_root: left.root.name.clone(),
            candidate_root: right.root.name.clone(),
            added,
            removed,
            grown,
            shrunk,
            net_bytes_delta,
        }
    }

    fn collect_files(root: &SnapshotNodeV2, map: &mut HashMap<String, u64>) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if !node.is_dir {
                map.insert(node.path.clone(), node.size);
            }
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
    }
}
