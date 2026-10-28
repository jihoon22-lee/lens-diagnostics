use lens_disk::{ArenaTree, DiskScanner, NodeId, ScanOptions, ScanResult};
use lens_log::LogIndexer;
use lens_net::{inspect_network, NetReport};
use lens_sys::SystemdUnit;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTab {
    Storage,
    Services,
    Logs,
    Network,
}

impl TuiTab {
    pub fn all() -> &'static [TuiTab] {
        &[
            TuiTab::Storage,
            TuiTab::Services,
            TuiTab::Logs,
            TuiTab::Network,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            TuiTab::Storage => "1: Storage",
            TuiTab::Services => "2: Services",
            TuiTab::Logs => "3: Logs",
            TuiTab::Network => "4: Network",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            TuiTab::Storage => TuiTab::Services,
            TuiTab::Services => TuiTab::Logs,
            TuiTab::Logs => TuiTab::Network,
            TuiTab::Network => TuiTab::Storage,
        }
    }
}

#[derive(Debug)]
pub struct TuiItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub allocated_size: u64,
    pub children_count: usize,
    /// Arena node this row came from — enables in-memory navigation
    /// without rescanning the directory.
    pub node_id: Option<NodeId>,
}

/// Result of a background directory scan.
type ScanOutcome = (PathBuf, Result<ScanResult, String>);

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub struct TuiApp {
    pub current_path: PathBuf,
    /// Root of the currently held arena tree.
    pub scan_root: PathBuf,
    tree: Option<ArenaTree>,
    /// Node ids from the tree root down to `current_path`'s node.
    nav_stack: Vec<NodeId>,
    scan_rx: Option<Receiver<ScanOutcome>>,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_worker: Option<std::thread::JoinHandle<()>>,
    pending_scan: Option<PathBuf>,
    pub scanning: bool,
    pub spinner_frame: usize,
    pub scan_errors: Vec<String>,
    pub scan_incomplete: bool,
    pub active_tab: TuiTab,
    pub items: Vec<TuiItem>,
    pub selected_index: usize,
    pub total_size: u64,
    pub net_report: Option<NetReport>,
    pub net_selected: usize,
    pub sys_units: Vec<SystemdUnit>,
    pub sys_error: Option<String>,
    pub sys_selected: usize,
    pub log_path: Option<PathBuf>,
    /// Explicit `--log` argument — beats the auto-detected candidates.
    log_override: Option<PathBuf>,
    pub log_lines: Vec<String>,
    pub log_scroll: usize,
    /// `/` search over the Logs tab; `None` shows all loaded lines.
    pub log_search: Option<String>,
    /// True while the `/` prompt is capturing input.
    pub search_active: bool,
    pub search_buf: String,
    pub show_help: bool,
    /// Content height of the active list — updated every draw so
    /// PgUp/PgDn move by a page.
    pub page_size: usize,
    pub status_message: String,
    pub should_quit: bool,
}

impl TuiApp {
    /// Start the app at `initial_path` with an explicit log file for the
    /// Logs tab (`None` auto-detects a system log). The directory scan
    /// runs on a background thread — the UI is interactive immediately
    /// and shows a spinner while `scanning` is true.
    pub fn new(initial_path: &Path, log: Option<PathBuf>) -> Self {
        // Canonicalize so "." becomes a real absolute path — otherwise
        // parent() yields Some("") and navigating up silently empties the
        // listing with "Failed to scan \"\"".
        let start = initial_path
            .canonicalize()
            .unwrap_or_else(|_| initial_path.to_path_buf());
        let mut app = Self {
            current_path: start.clone(),
            scan_root: start.clone(),
            tree: None,
            nav_stack: Vec::new(),
            scan_rx: None,
            scan_cancel: None,
            scan_worker: None,
            pending_scan: None,
            scanning: false,
            spinner_frame: 0,
            scan_errors: Vec::new(),
            scan_incomplete: false,
            active_tab: TuiTab::Storage,
            items: Vec::new(),
            selected_index: 0,
            total_size: 0,
            net_report: None,
            net_selected: 0,
            sys_units: Vec::new(),
            sys_error: None,
            sys_selected: 0,
            log_path: None,
            log_override: log,
            log_lines: Vec::new(),
            log_scroll: 0,
            log_search: None,
            search_active: false,
            search_buf: String::new(),
            show_help: false,
            page_size: 10,
            status_message:
                "Ready. [Tab/1-4] Switch tabs  [j/k] Navigate  [Enter] Open  [?] Help  [q] Quit"
                    .to_string(),
            should_quit: false,
        };
        app.request_scan(start);
        app.reload_network();
        app.reload_services();
        app.reload_logs();
        app
    }

    /// Kick off a background scan of `path`. The result is picked up by
    /// `poll_scan` on the next frame; until then the previous listing
    /// stays visible with a scanning indicator.
    pub fn request_scan(&mut self, path: PathBuf) {
        self.cancel_scan();
        self.scanning = true;
        self.status_message = format!("Scanning {} …", path.display());
        if self
            .scan_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            // Coalesce rapid navigation into one pending request. Never spawn
            // another worker until cancellation of the previous one finishes.
            self.pending_scan = Some(path);
            return;
        }
        self.start_scan(path);
    }

    fn start_scan(&mut self, path: PathBuf) {
        if let Some(worker) = self.scan_worker.take() {
            let _ = worker.join();
        }
        let token = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(token.clone());
        let (tx, rx) = mpsc::channel();
        let target = path.clone();
        self.scan_worker = Some(std::thread::spawn(move || {
            let res = DiskScanner::new(ScanOptions::default())
                .with_cancellation(token)
                .scan(&target)
                .map_err(|e| e.to_string());
            let _ = tx.send((target, res));
        }));
        self.scan_rx = Some(rx);
        self.scanning = true;
        self.spinner_frame = 0;
        self.status_message = format!("Scanning {} …", path.display());
    }

    /// Drop the old receiver as well as cancelling its worker: late results
    /// cannot replace a more recently requested directory.
    pub fn cancel_scan(&mut self) {
        self.pending_scan = None;
        if let Some(token) = self.scan_cancel.take() {
            token.store(true, Ordering::Relaxed);
        }
        self.scan_rx = None;
        if self.scanning {
            self.status_message = "Scan cancelled; previous listing retained".into();
        }
        self.scanning = false;
    }

    /// Drain a finished background scan and adopt its tree. Called once
    /// per event-loop iteration.
    pub fn poll_scan(&mut self) {
        if self.pending_scan.is_some() {
            if self
                .scan_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                return;
            }
            let path = self.pending_scan.take().unwrap();
            self.start_scan(path);
        }
        let rx = match self.scan_rx.as_ref() {
            Some(rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok((path, Ok(res))) => self.adopt_scan(path, res),
            Ok((path, Err(e))) => {
                self.scanning = false;
                self.scan_rx = None;
                self.items.clear();
                self.total_size = 0;
                self.selected_index = 0;
                self.scan_errors = vec![e.clone()];
                self.scan_incomplete = true;
                self.status_message = format!("Failed to scan {:?}: {}", path, e);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.scanning = false;
                self.scan_rx = None;
            }
        }
    }

    /// Block until the in-flight scan finishes (tests and synchronous
    /// callers); no-op when nothing is scanning.
    pub fn wait_scan(&mut self) {
        if let Some(path) = self.pending_scan.take() {
            if let Some(worker) = self.scan_worker.take() {
                let _ = worker.join();
            }
            self.start_scan(path);
        }
        if let Some(rx) = self.scan_rx.take() {
            if let Ok((path, res)) = rx.recv() {
                match res {
                    Ok(res) => self.adopt_scan(path, res),
                    Err(e) => {
                        self.scanning = false;
                        self.scan_errors = vec![e.clone()];
                        self.scan_incomplete = true;
                        self.status_message = format!("Failed to scan {:?}: {}", path, e);
                    }
                }
            } else {
                self.scanning = false;
            }
        }
    }

    fn adopt_scan(&mut self, path: PathBuf, res: ScanResult) {
        self.scanning = false;
        self.scan_rx = None;
        self.scan_root = path;
        self.current_path = self.scan_root.clone();
        self.total_size = if res.tree.nodes.is_empty() {
            0
        } else {
            res.tree.nodes[res.root_id as usize].size
        };
        self.scan_errors = res.errors.clone();
        self.scan_incomplete = !res.complete || res.truncated || !res.errors.is_empty();
        self.nav_stack = vec![res.root_id];
        self.tree = Some(res.tree);
        self.selected_index = 0;
        self.populate_items();
        self.status_message = if self.scan_incomplete {
            format!(
                "Scan INCOMPLETE: {} error(s){}",
                self.scan_errors.len(),
                if res.truncated { ", truncated" } else { "" }
            )
        } else {
            format!("Scanned {} entries", res.scanned_entries)
        };
    }

    /// Spinner glyph for the current frame — the caller bumps
    /// `spinner_frame` each draw while `scanning`.
    pub fn spinner(&self) -> char {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
    }

    pub fn set_tab(&mut self, tab: TuiTab) {
        self.active_tab = tab;
    }

    pub fn reload_network(&mut self) {
        match inspect_network(None) {
            Ok(rep) => {
                if self.net_selected >= rep.listening.len() {
                    self.net_selected = 0;
                }
                self.net_report = Some(rep);
            }
            Err(_) => {
                // Clear stale data so the UI shows "unavailable", not
                // a snapshot from a previous run.
                self.net_report = None;
                self.net_selected = 0;
            }
        }
    }

    /// Load systemd units (with drop-ins) for the Services tab. Uses the
    /// full systemd search path so vendor units under /usr/lib and runtime
    /// units under /run are visible, not just /etc.
    pub fn reload_services(&mut self) {
        let dirs: Vec<PathBuf> = lens_sys::SYSTEMD_SEARCH_DIRS
            .iter()
            .map(PathBuf::from)
            .collect();
        match lens_sys::load_units_merged(&dirs) {
            Ok(units) => {
                self.sys_units = units.into_values().collect();
                self.sys_error = None;
            }
            Err(error) => {
                self.sys_units.clear();
                self.sys_error = Some(error.to_string());
            }
        }
        if self.sys_selected >= self.sys_units.len() {
            self.sys_selected = 0;
        }
    }

    /// Tail the Logs tab source: an explicit `--log` path wins; otherwise
    /// the first readable system log.
    pub fn reload_logs(&mut self) {
        let path = self.log_override.clone().or_else(|| {
            let candidates = [
                "/var/log/syslog",
                "/var/log/messages",
                "/var/log/kern.log",
                "/var/log/daemon.log",
            ];
            candidates
                .iter()
                .map(Path::new)
                .find(|p| p.is_file())
                .map(|p| p.to_path_buf())
                .or_else(|| {
                    // Fall back to the first *.log file in /var/log.
                    std::fs::read_dir("/var/log").ok().and_then(|rd| {
                        rd.flatten().map(|e| e.path()).find(|p| {
                            p.extension().map(|e| e == "log").unwrap_or(false) && p.is_file()
                        })
                    })
                })
        });

        let loaded = path.map(|p| match LogIndexer::open(&p) {
            Ok(indexer) => {
                let start = indexer.len().saturating_sub(2000);
                // Lossy: lines with invalid UTF-8 are shown with U+FFFD
                // instead of being silently dropped from the tail.
                let lines: Vec<String> = (start..indexer.len())
                    .filter_map(|i| indexer.get_line_lossy(i).map(|s| s.into_owned()))
                    .collect();
                Ok((p, lines))
            }
            Err(e) => Err((p, e)),
        });
        match loaded {
            Some(Ok((p, lines))) => {
                self.log_lines = lines;
                self.log_path = Some(p);
                self.log_scroll = self.log_lines.len();
            }
            res => {
                // No readable log — drop previously shown lines so the tab
                // doesn't display stale output under a new directory.
                self.log_lines.clear();
                self.log_path = None;
                self.log_scroll = 0;
                if let Some(Err((p, e))) = res {
                    // An explicit --log path (or a discovered file) that
                    // failed to open tells the user why the tab is empty.
                    self.status_message = format!("Cannot read log file {:?}: {}", p, e);
                } else if let Some(p) = &self.log_override {
                    self.status_message = format!("Cannot read log file {:?}", p);
                }
            }
        }
    }

    /// Force a fresh reload of the active tab (or background scan on Storage).
    pub fn reload(&mut self) {
        match self.active_tab {
            TuiTab::Storage => self.request_scan(self.current_path.clone()),
            TuiTab::Services => self.reload_services(),
            TuiTab::Logs => self.reload_logs(),
            TuiTab::Network => self.reload_network(),
        }
    }

    fn current_node_id(&self) -> Option<NodeId> {
        self.nav_stack.last().copied()
    }

    fn populate_items(&mut self) {
        self.items.clear();
        let (Some(tree), Some(current)) = (self.tree.as_ref(), self.current_node_id()) else {
            return;
        };
        for child_id in tree.children_ids(current) {
            let node = &tree.nodes[child_id as usize];
            let sub_children_count = tree.children_ids(child_id).len();
            self.items.push(TuiItem {
                name: node.name.clone(),
                path: self.current_path.join(&node.name),
                is_dir: node.is_dir,
                size: node.size,
                allocated_size: node.allocated_size,
                children_count: sub_children_count,
                node_id: Some(child_id),
            });
        }

        self.items.sort_by_key(|b| std::cmp::Reverse(b.size));
        if self.selected_index >= self.items.len() {
            self.selected_index = 0;
        }
    }

    pub fn next(&mut self) {
        match self.active_tab {
            TuiTab::Storage => {
                if !self.items.is_empty() {
                    self.selected_index = (self.selected_index + 1) % self.items.len();
                }
            }
            TuiTab::Services => {
                if !self.sys_units.is_empty() {
                    self.sys_selected = (self.sys_selected + 1) % self.sys_units.len();
                }
            }
            TuiTab::Logs => {
                let len = self.visible_log_count();
                if self.log_scroll < len {
                    self.log_scroll += 1;
                }
            }
            TuiTab::Network => {
                if let Some(rep) = &self.net_report {
                    if !rep.listening.is_empty() {
                        self.net_selected = (self.net_selected + 1) % rep.listening.len();
                    }
                }
            }
        }
    }

    pub fn previous(&mut self) {
        match self.active_tab {
            TuiTab::Storage => {
                if !self.items.is_empty() {
                    if self.selected_index == 0 {
                        self.selected_index = self.items.len() - 1;
                    } else {
                        self.selected_index -= 1;
                    }
                }
            }
            TuiTab::Services => {
                if !self.sys_units.is_empty() {
                    if self.sys_selected == 0 {
                        self.sys_selected = self.sys_units.len() - 1;
                    } else {
                        self.sys_selected -= 1;
                    }
                }
            }
            TuiTab::Logs => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            TuiTab::Network => {
                if let Some(rep) = &self.net_report {
                    if !rep.listening.is_empty() {
                        if self.net_selected == 0 {
                            self.net_selected = rep.listening.len() - 1;
                        } else {
                            self.net_selected -= 1;
                        }
                    }
                }
            }
        }
    }

    /// Page down on the active tab's selection/scroll position.
    pub fn page_down(&mut self) {
        let page = self.page_size.max(1);
        match self.active_tab {
            TuiTab::Storage => {
                if !self.items.is_empty() {
                    self.selected_index = (self.selected_index + page).min(self.items.len() - 1);
                }
            }
            TuiTab::Services => {
                if !self.sys_units.is_empty() {
                    self.sys_selected = (self.sys_selected + page).min(self.sys_units.len() - 1);
                }
            }
            TuiTab::Logs => {
                self.log_scroll = (self.log_scroll + page).min(self.visible_log_count());
            }
            TuiTab::Network => {
                if let Some(rep) = &self.net_report {
                    if !rep.listening.is_empty() {
                        self.net_selected = (self.net_selected + page).min(rep.listening.len() - 1);
                    }
                }
            }
        }
    }

    pub fn page_up(&mut self) {
        let page = self.page_size.max(1);
        match self.active_tab {
            TuiTab::Storage => self.selected_index = self.selected_index.saturating_sub(page),
            TuiTab::Services => self.sys_selected = self.sys_selected.saturating_sub(page),
            TuiTab::Logs => self.log_scroll = self.log_scroll.saturating_sub(page),
            TuiTab::Network => self.net_selected = self.net_selected.saturating_sub(page),
        }
    }

    /// `g` — first entry/top.
    pub fn jump_first(&mut self) {
        match self.active_tab {
            TuiTab::Storage => self.selected_index = 0,
            TuiTab::Services => self.sys_selected = 0,
            TuiTab::Logs => {
                self.log_scroll = self.page_size.max(1).min(self.visible_log_count());
            }
            TuiTab::Network => self.net_selected = 0,
        }
    }

    /// `G` — last entry/bottom.
    pub fn jump_last(&mut self) {
        match self.active_tab {
            TuiTab::Storage => {
                if !self.items.is_empty() {
                    self.selected_index = self.items.len() - 1;
                }
            }
            TuiTab::Services => {
                if !self.sys_units.is_empty() {
                    self.sys_selected = self.sys_units.len() - 1;
                }
            }
            TuiTab::Logs => self.log_scroll = self.visible_log_count(),
            TuiTab::Network => {
                if let Some(rep) = &self.net_report {
                    if !rep.listening.is_empty() {
                        self.net_selected = rep.listening.len() - 1;
                    }
                }
            }
        }
    }

    /// Log lines after applying the `/` search filter (indices into
    /// `log_lines`, preserving line order).
    pub fn visible_log_indices(&self) -> Vec<usize> {
        match &self.log_search {
            Some(q) if !q.is_empty() => (0..self.log_lines.len())
                .filter(|&i| self.log_lines[i].contains(q.as_str()))
                .collect(),
            _ => (0..self.log_lines.len()).collect(),
        }
    }

    fn visible_log_count(&self) -> usize {
        self.visible_log_indices().len()
    }

    /// Enter the selected directory. Descends inside the already-scanned
    /// arena tree — no rescan — unless the node was not fully scanned
    /// (error/truncated), in which case a background scan is requested.
    pub fn enter(&mut self) {
        let Some(item) = self.items.get(self.selected_index) else {
            return;
        };
        if !item.is_dir {
            return;
        }
        let target = item.path.clone();
        let in_tree = match (self.tree.as_ref(), item.node_id) {
            (Some(tree), Some(id)) => {
                let node = &tree.nodes[id as usize];
                node.complete && node.error.is_empty()
            }
            _ => false,
        };
        if in_tree {
            let id = item.node_id.unwrap();
            if let Some(tree) = self.tree.as_ref() {
                self.total_size = tree.nodes[id as usize].size;
            }
            self.nav_stack.push(id);
            self.current_path = target;
            self.selected_index = 0;
            self.populate_items();
        } else {
            self.request_scan(target);
        }
    }

    /// Go to the parent directory. In-memory while inside the scanned
    /// tree; ascending past the scan root requests a background scan.
    pub fn parent(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
            if let (Some(tree), Some(id)) = (self.tree.as_ref(), self.current_node_id()) {
                let node = &tree.nodes[id as usize];
                self.current_path = self.scan_root.join(&node.rel_path);
                self.total_size = node.size;
            }
            self.selected_index = 0;
            self.populate_items();
        } else if let Some(p) = self.current_path.parent() {
            self.request_scan(p.to_path_buf());
        }
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        self.cancel_scan();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_tui_app_navigation() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("file.txt"), "hello").expect("write");

        let mut app = TuiApp::new(dir.path(), None);
        app.wait_scan();
        assert_eq!(app.items.len(), 2);

        app.next();
        assert_eq!(app.selected_index, 1);
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_parent_from_relative_start() {
        // `lens tui` starts at "."; Path::new(".").parent() is Some(""), an
        // unscannable path. Canonicalization makes Backspace reach the real
        // parent directory instead.
        let cwd = std::env::current_dir().unwrap();
        let mut app = TuiApp::new(Path::new("."), None);
        assert_eq!(app.current_path, cwd);
        app.wait_scan();

        // Ascending past the scan root requests a background scan of the
        // parent; adopt it before asserting.
        app.parent();
        app.wait_scan();
        assert_eq!(app.current_path, cwd.parent().unwrap().to_path_buf());
        assert!(!app.status_message.contains("Failed to scan"));
    }

    #[test]
    fn test_in_memory_descend_and_ascend() {
        let dir = tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("mkdir");
        std::fs::write(sub.join("inner.txt"), "x").expect("write");

        let mut app = TuiApp::new(dir.path(), None);
        app.wait_scan();
        assert_eq!(app.items.len(), 1);
        assert_eq!(app.items[0].name, "sub");

        // Descend without rescanning: nav stack grows, listing comes from
        // the already-scanned tree.
        app.enter();
        assert_eq!(app.current_path, sub);
        assert_eq!(app.items.len(), 1);
        assert_eq!(app.items[0].name, "inner.txt");
        assert!(!app.scanning);

        // Ascend back in memory.
        app.parent();
        assert_eq!(app.current_path, dir.path());
        assert_eq!(app.items.len(), 1);
        assert!(!app.scanning);
    }

    #[test]
    fn test_log_search_filter() {
        let dir = tempdir().expect("tempdir");
        let log = dir.path().join("app.log");
        std::fs::write(&log, "INFO ok\nERROR boom\nINFO fine\n").unwrap();

        let mut app = TuiApp::new(dir.path(), Some(log));
        app.wait_scan();
        assert_eq!(app.log_lines.len(), 3);

        app.log_search = Some("ERROR".to_string());
        let idx = app.visible_log_indices();
        assert_eq!(idx, vec![1]);
    }

    #[test]
    fn test_page_navigation() {
        let dir = tempdir().expect("tempdir");
        for i in 0..30 {
            std::fs::write(dir.path().join(format!("f{:02}.txt", i)), "x").unwrap();
        }
        let mut app = TuiApp::new(dir.path(), None);
        app.wait_scan();
        app.page_size = 10;
        app.page_down();
        assert_eq!(app.selected_index, 10);
        app.jump_last();
        assert_eq!(app.selected_index, 29);
        app.jump_first();
        assert_eq!(app.selected_index, 0);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    fn rapid_requests_coalesce_and_cancel_pending_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(dir.path(), None);
        app.wait_scan();
        // Keep a worker alive deterministically while requests arrive.
        let (release, wait) = std::sync::mpsc::channel::<()>();
        app.scan_worker = Some(std::thread::spawn(move || {
            let _ = wait.recv();
        }));
        for n in 0..1000 {
            app.request_scan(dir.path().join(n.to_string()));
        }
        assert_eq!(app.pending_scan, Some(dir.path().join("999")));
        assert!(app.scan_rx.is_none());
        app.cancel_scan();
        assert!(app.pending_scan.is_none());
        release.send(()).unwrap();
        app.scan_worker.take().unwrap().join().unwrap();
        app.request_scan(dir.path().to_path_buf());
        app.wait_scan();
        assert_eq!(app.scan_root, dir.path());
    }

    #[test]
    fn new_request_discards_old_worker_receiver() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(first.path(), None);
        let old_token = app.scan_cancel.clone().unwrap();
        app.request_scan(second.path().to_path_buf());
        assert!(old_token.load(Ordering::Relaxed));
        app.wait_scan();
        assert_eq!(app.scan_root, second.path());
        app.poll_scan();
        assert_eq!(app.scan_root, second.path());
    }
}
