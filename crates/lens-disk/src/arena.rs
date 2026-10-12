use lens_core::FileIdentity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsKind {
    RegularFile,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskMetadata {
    pub kind: FsKind,
    pub identity: FileIdentity,
    pub logical_size: u64,
    pub allocated_size: u64,
    pub hard_link_count: u64,
    pub hard_link_count_known: bool,
    pub permissions: u32,
    pub permissions_known: bool,
    pub owner: u64,
    pub group: u64,
    pub ownership_known: bool,
    pub modified_ns: i64,
    pub modified_time_known: bool,
}

impl Default for DiskMetadata {
    fn default() -> Self {
        Self {
            kind: FsKind::Other,
            identity: FileIdentity {
                device: 0,
                inode: 0,
                mode: 0,
                size: 0,
                modified_sec: 0,
                modified_nsec: 0,
                changed_sec: 0,
                changed_nsec: 0,
            },
            logical_size: 0,
            allocated_size: 0,
            hard_link_count: 1,
            hard_link_count_known: true,
            permissions: 0,
            permissions_known: false,
            owner: 0,
            group: 0,
            ownership_known: false,
            modified_ns: 0,
            modified_time_known: false,
        }
    }
}

pub type NodeId = u32;

/// Memory-efficient compact node stored in a contiguous Arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsNode {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub allocated_size: u64,
    pub reclaimable_size: u64,
    pub logical_size_known: bool,
    pub allocated_size_known: bool,
    pub reclaimable_size_known: bool,
    pub first_child: Option<NodeId>,
    /// Tail of the child list so `add_child` is O(1) instead of walking
    /// every sibling for large directories.
    pub last_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub metadata: DiskMetadata,
    pub target_metadata: Option<DiskMetadata>,
    pub followed: bool,
    pub cycle_skipped: bool,
    pub mount_boundary_skipped: bool,
    pub complete: bool,
    pub error: String,
}

impl FsNode {
    pub fn new(name: String, rel_path: String, is_dir: bool) -> Self {
        Self {
            name,
            rel_path,
            is_dir,
            size: 0,
            allocated_size: 0,
            reclaimable_size: 0,
            logical_size_known: true,
            allocated_size_known: true,
            reclaimable_size_known: true,
            first_child: None,
            last_child: None,
            next_sibling: None,
            metadata: DiskMetadata::default(),
            target_metadata: None,
            followed: false,
            cycle_skipped: false,
            mount_boundary_skipped: false,
            complete: true,
            error: String::new(),
        }
    }
}

/// Contiguous Arena Tree that avoids pointer chasing and heap fragmentation.
#[derive(Debug, Clone, Default)]
pub struct ArenaTree {
    pub nodes: Vec<FsNode>,
}

impl ArenaTree {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(capacity),
        }
    }

    pub fn alloc(&mut self, node: FsNode) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(node);
        id
    }

    pub fn get(&self, id: NodeId) -> Option<&FsNode> {
        self.nodes.get(id as usize)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut FsNode> {
        self.nodes.get_mut(id as usize)
    }

    pub fn add_child(&mut self, parent_id: NodeId, child_id: NodeId) {
        debug_assert!((child_id as usize) < self.nodes.len());
        // Clear any stale sibling link so re-parenting cannot splice an old
        // chain (or a cycle) into this parent's list.
        if let Some(c) = self.nodes.get_mut(child_id as usize) {
            c.next_sibling = None;
        }
        let tail = match self.nodes.get(parent_id as usize) {
            Some(p) => p.last_child,
            None => return,
        };
        match tail {
            None => self.nodes[parent_id as usize].first_child = Some(child_id),
            Some(t) => self.nodes[t as usize].next_sibling = Some(child_id),
        }
        self.nodes[parent_id as usize].last_child = Some(child_id);
    }

    pub fn children_ids(&self, parent_id: NodeId) -> Vec<NodeId> {
        let mut ids = Vec::new();
        if let Some(parent) = self.get(parent_id) {
            let mut curr = parent.first_child;
            while let Some(id) = curr {
                ids.push(id);
                curr = self.nodes[id as usize].next_sibling;
            }
        }
        ids
    }

    /// Aggregates sizes post-order from leaves up to the root.
    /// Iterative so arbitrarily deep trees cannot overflow the call stack.
    /// Hardlinks are counted once: a regular file whose inode reports
    /// `hard_link_count > 1` contributes its size only at its first
    /// occurrence in node-id order — the node itself keeps its own
    /// size, directory totals just don't double-count it. Nodes with an
    /// unknown (0,0) identity are never deduplicated.
    pub fn aggregate_sizes(&mut self, root_id: NodeId) -> u64 {
        let mut sizes = vec![0u64; self.nodes.len()];
        // Pre-pass: mark hardlink occurrences after the first so their
        // contribution can be skipped in the child sums.
        let mut counted: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
        let mut dup_inode = vec![false; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            let id = &n.metadata.identity;
            if n.is_dir
                || n.metadata.kind != FsKind::RegularFile
                || (id.device, id.inode) == (0, 0)
                || n.metadata.hard_link_count <= 1
            {
                continue;
            }
            if !counted.insert((id.device, id.inode)) {
                dup_inode[i] = true;
            }
        }
        // (node, children_processed)
        let mut stack: Vec<(NodeId, bool)> = vec![(root_id, false)];
        while let Some((id, processed)) = stack.pop() {
            if !processed {
                stack.push((id, true));
                let mut child = self.nodes[id as usize].first_child;
                while let Some(c) = child {
                    stack.push((c, false));
                    child = self.nodes[c as usize].next_sibling;
                }
                continue;
            }
            let mut total_size = 0u64;
            let mut total_alloc = 0u64;
            let mut child = self.nodes[id as usize].first_child;
            while let Some(c) = child {
                if !dup_inode[c as usize] {
                    total_size = total_size.saturating_add(sizes[c as usize]);
                    total_alloc = total_alloc.saturating_add(self.nodes[c as usize].allocated_size);
                }
                child = self.nodes[c as usize].next_sibling;
            }
            let node = &mut self.nodes[id as usize];
            if node.is_dir {
                node.size = total_size;
                node.allocated_size = total_alloc;
            }
            sizes[id as usize] = node.size;
        }
        self.nodes[root_id as usize].size
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
