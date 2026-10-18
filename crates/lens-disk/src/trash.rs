use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

use lens_core::{FileIdentity, LensError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashReceipt {
    pub original_path: PathBuf,
    pub trashed_file_path: PathBuf,
    pub info_path: PathBuf,
    pub deletion_date: String,
    pub size: u64,
    pub identity: FileIdentity,
}

/// One row of `lens disk trash-list`: a `.trashinfo` entry in the trash.
#[derive(Debug, Clone, Serialize)]
pub struct TrashEntry {
    /// Trash-internal name (the file inside `files/`).
    pub name: String,
    pub original_path: PathBuf,
    pub deletion_date: String,
    /// `files/<name>` exists — a dangling info file lists as false.
    pub present: bool,
    /// Whether the lens identity sidecar exists (foreign entries lack it).
    pub has_identity: bool,
}

/// Percent-encode a path per the FreeDesktop Trash spec (`Path=` is a
/// URI-style value: reserved bytes are `%XX` escaped). Unreserved +
/// `-._~/` are passed through.
fn encode_trashinfo_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Decode a `Path=` value from a `.trashinfo` (inverse of
/// `encode_trashinfo_path`).
fn decode_trashinfo_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn identity_matches(a: &FileIdentity, b: &FileIdentity) -> bool {
    // ctime changes on rename, so it is deliberately excluded.
    a.device == b.device
        && a.inode == b.inode
        && a.mode == b.mode
        && a.size == b.size
        && a.modified_sec == b.modified_sec
        && a.modified_nsec == b.modified_nsec
}

fn occupied(path: &Path) -> bool {
    // lstat semantics: a dangling symlink still occupies the name.
    path.symlink_metadata().is_ok()
}

pub struct TrashManager {
    trash_dir: PathBuf,
}

impl Default for TrashManager {
    fn default() -> Self {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local/share")
            });
        Self {
            trash_dir: base.join("Trash"),
        }
    }
}

impl TrashManager {
    pub fn new(trash_dir: PathBuf) -> Self {
        Self { trash_dir }
    }

    pub fn files_dir(&self) -> PathBuf {
        self.trash_dir.join("files")
    }

    pub fn info_dir(&self) -> PathBuf {
        self.trash_dir.join("info")
    }

    pub fn ensure_directories(&self) -> Result<()> {
        #[cfg(unix)]
        let is_topdir = self
            .trash_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(".Trash-"))
            .unwrap_or(false);

        #[cfg(unix)]
        if is_topdir {
            if let Ok(sym_meta) = fs::symlink_metadata(&self.trash_dir) {
                if sym_meta.file_type().is_symlink() {
                    return Err(LensError::InvalidInput {
                        message: format!(
                            "Refusing to use symlinked topdir trash {:?}",
                            self.trash_dir
                        ),
                    });
                }
            }
        }

        fs::create_dir_all(self.files_dir()).map_err(|e| LensError::Io {
            path: self.files_dir(),
            source: e,
        })?;
        fs::create_dir_all(self.info_dir()).map_err(|e| LensError::Io {
            path: self.info_dir(),
            source: e,
        })?;
        // FreeDesktop: a topdir `.Trash-$uid` directory must not be a symlink,
        // must be owned by current euid, and must be mode 0700.
        #[cfg(unix)]
        if is_topdir {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let sym_meta = fs::symlink_metadata(&self.trash_dir).map_err(|e| LensError::Io {
                path: self.trash_dir.clone(),
                source: e,
            })?;
            if sym_meta.file_type().is_symlink() {
                return Err(LensError::InvalidInput {
                    message: format!(
                        "Refusing to use symlinked topdir trash {:?}",
                        self.trash_dir
                    ),
                });
            }
            let euid = unsafe { libc::geteuid() };
            if sym_meta.uid() != euid {
                return Err(LensError::InvalidInput {
                    message: format!(
                        "Topdir trash {:?} is not owned by current user (uid {} != {})",
                        self.trash_dir,
                        sym_meta.uid(),
                        euid
                    ),
                });
            }
            let mut perms = sym_meta.permissions();
            perms.set_mode(0o700);
            let _ = fs::set_permissions(&self.trash_dir, perms);
        }
        Ok(())
    }

    /// The default (home) trash — `$XDG_DATA_HOME/Trash` or
    /// `~/.local/share/Trash`.
    pub fn home_trash() -> Self {
        Self::default()
    }

    /// The FreeDesktop topdir trash for the filesystem containing `path`:
    /// `$topdir/.Trash-$uid` with mode 0700.
    pub fn for_topdir_of(path: &Path) -> Result<Self> {
        let original = std::path::absolute(path).map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let meta = fs::symlink_metadata(&original).map_err(|e| LensError::Io {
            path: original.clone(),
            source: e,
        })?;
        #[cfg(unix)]
        let dev = {
            use std::os::unix::fs::MetadataExt;
            meta.dev()
        };
        #[cfg(not(unix))]
        let dev = 0u64;

        // Walk ancestors until st_dev changes: the last dir on the file's
        // filesystem is its topdir (mount point).
        let mut topdir = original
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mut cur = topdir.clone();
            while let Some(parent) = cur.parent() {
                match fs::metadata(parent) {
                    Ok(m) if m.dev() == dev => cur = parent.to_path_buf(),
                    _ => break,
                }
            }
            topdir = cur;
        }

        let uid = unsafe { libc::geteuid() };
        let trash_dir = topdir.join(format!(".Trash-{}", uid));
        Ok(Self { trash_dir })
    }

    /// Entry list of this trash for `trash list`: one row per
    /// `info/*.trashinfo`, in name order.
    pub fn list(&self) -> Result<Vec<TrashEntry>> {
        let info_dir = self.info_dir();
        if !info_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut names: Vec<PathBuf> = fs::read_dir(&info_dir)
            .map_err(|e| LensError::Io {
                path: info_dir.clone(),
                source: e,
            })?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".trashinfo"))
                    .unwrap_or(false)
            })
            .collect();
        names.sort();
        let mut out = Vec::new();
        for info_path in names {
            let Some(name) = info_path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".trashinfo"))
                .map(str::to_string)
            else {
                continue;
            };
            let content = fs::read_to_string(&info_path).unwrap_or_default();
            let mut original = PathBuf::new();
            let mut deletion_date = String::new();
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("Path=") {
                    original = PathBuf::from(decode_trashinfo_path(v));
                } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                    deletion_date = v.to_string();
                }
            }
            out.push(TrashEntry {
                present: occupied(&self.files_dir().join(&name)),
                has_identity: self.identity_path(&name).exists(),
                name,
                original_path: original,
                deletion_date,
            });
        }
        Ok(out)
    }

    /// Restore `name` (a `trash list` name) back to its recorded original
    /// path. Fails closed: refuses overwrite and, when the identity
    /// sidecar exists, refuses a file whose metadata changed in trash.
    pub fn restore_by_name(&self, name: &str) -> Result<TrashReceipt> {
        // `name` is a trash-internal key — reject traversal outright while
        // permitting legitimate dotfiles (`.env`, `.bashrc`).
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0')
        {
            return Err(LensError::InvalidInput {
                message: format!("invalid trash entry name {:?}", name),
            });
        }
        let info_path = self.info_dir().join(format!("{}.trashinfo", name));
        let trashed_file_path = self.files_dir().join(name);
        let content = fs::read_to_string(&info_path).map_err(|e| LensError::Io {
            path: info_path.clone(),
            source: e,
        })?;
        let mut original = None;
        let mut deletion_date = String::new();
        for line in content.lines() {
            if let Some(v) = line.strip_prefix("Path=") {
                original = Some(PathBuf::from(decode_trashinfo_path(v)));
            } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                deletion_date = v.to_string();
            }
        }
        let original_path = original.ok_or_else(|| LensError::InvalidInput {
            message: format!("trashinfo {:?} has no Path=", info_path),
        })?;

        // The sidecar holds the identity recorded at trash time.
        let identity = self.read_identity(name)?;
        let has_identity = identity.is_some();
        let receipt = TrashReceipt {
            original_path,
            trashed_file_path,
            info_path,
            deletion_date,
            size: identity.as_ref().map(|r| r.size).unwrap_or(0),
            identity: identity.map(|r| r.identity).unwrap_or_default(),
        };
        // Fail-closed checks live in restore_common; the identity check
        // runs only when the sidecar exists (foreign trash entries lack it).
        self.restore_common(&receipt, has_identity)?;
        Ok(receipt)
    }

    fn identity_path(&self, name: &str) -> PathBuf {
        self.info_dir().join(format!("{}.lens.json", name))
    }

    fn write_identity(&self, name: &str, receipt: &TrashReceipt) {
        use std::io::Write;
        let p = self.identity_path(name);
        if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            let _ = f.write_all(
                serde_json::to_string(&receipt)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
    }

    fn read_identity(&self, name: &str) -> Result<Option<TrashReceipt>> {
        let p = self.identity_path(name);
        match fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| LensError::InvalidInput {
                    message: format!("invalid identity sidecar {:?}: {}", p, e),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LensError::Io { path: p, source: e }),
        }
    }

    /// Moves a file to trash following the FreeDesktop Trash Specification.
    /// Symlinks are moved as links — the target is never touched. On EXDEV
    /// (file lives on a different filesystem than the home trash) falls
    /// back to the mount's `$topdir/.Trash-$uid`.
    pub fn move_to_trash<P: AsRef<Path>>(&self, path: P) -> Result<TrashReceipt> {
        let original = std::path::absolute(path.as_ref()).map_err(|e| LensError::Io {
            path: path.as_ref().to_path_buf(),
            source: e,
        })?;
        match self.move_to_trash_inner(&original) {
            Err(LensError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::CrossesDevices =>
            {
                Self::for_topdir_of(&original)?.move_to_trash_inner(&original)
            }
            r => r,
        }
    }

    fn move_to_trash_inner(&self, original: &Path) -> Result<TrashReceipt> {
        self.ensure_directories()?;

        let meta = fs::symlink_metadata(original).map_err(|e| LensError::Io {
            path: original.to_path_buf(),
            source: e,
        })?;
        let identity = FileIdentity::from_metadata(&meta);
        let file_name = original
            .file_name()
            .ok_or_else(|| LensError::InvalidInput {
                message: "Cannot trash filesystem root".to_string(),
            })?
            .to_string_lossy();

        // Generate a unique destination name in trash (lstat checks, so a
        // dangling symlink still counts as occupied).
        let mut candidate_name = file_name.to_string();
        let mut counter = 1;
        while occupied(&self.files_dir().join(&candidate_name))
            || occupied(
                &self
                    .info_dir()
                    .join(format!("{}.trashinfo", candidate_name)),
            )
            || occupied(&self.identity_path(&candidate_name))
        {
            candidate_name = format!("{}_{}", file_name, counter);
            counter += 1;
        }

        let trashed_file_path = self.files_dir().join(&candidate_name);
        let info_path = self
            .info_dir()
            .join(format!("{}.trashinfo", candidate_name));

        // FreeDesktop `DeletionDate`: local time in `YYYY-MM-DDTHH:MM:SS`.
        let deletion_date = lens_core::time::local_now_iso();

        // 1. Write the .trashinfo file (create_new: never follow a planted
        // symlink or clobber an existing entry).
        let info_content = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode_trashinfo_path(original),
            deletion_date
        );
        {
            use std::io::Write;
            let mut info_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)
                .map_err(|e| LensError::Io {
                    path: info_path.clone(),
                    source: e,
                })?;
            info_file
                .write_all(info_content.as_bytes())
                .map_err(|e| LensError::Io {
                    path: info_path.clone(),
                    source: e,
                })?;
        }

        let receipt = TrashReceipt {
            original_path: original.to_path_buf(),
            trashed_file_path,
            info_path,
            deletion_date,
            size: identity.size,
            identity,
        };

        // 2. Move the file into files/. `rename` on a symlink moves the
        // link itself. (Note: POSIX rename is replace-on-race; the lstat
        // check above narrows but does not eliminate that window.)
        if let Err(e) = fs::rename(original, &receipt.trashed_file_path) {
            // Attempt clean rollback of info file if rename fails
            let _ = fs::remove_file(&receipt.info_path);
            return Err(LensError::Io {
                path: original.to_path_buf(),
                source: e,
            });
        }

        // 3. Record the lens identity sidecar so `trash restore` can
        // verify the file was not swapped while in trash.
        self.write_identity(&candidate_name, &receipt);

        Ok(receipt)
    }

    /// Restores a trashed file back to its original location.
    pub fn restore(&self, receipt: &TrashReceipt) -> Result<()> {
        self.restore_common(receipt, true)
    }

    fn restore_common(&self, receipt: &TrashReceipt, check_identity: bool) -> Result<()> {
        // Receipts must reference a file inside this trash's files/ dir and
        // a clean absolute original path.
        if !receipt.trashed_file_path.starts_with(self.files_dir()) {
            return Err(LensError::InvalidInput {
                message: format!(
                    "Trashed path {:?} is outside {}",
                    receipt.trashed_file_path,
                    self.files_dir().display()
                ),
            });
        }
        if !receipt.original_path.is_absolute()
            || receipt
                .original_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(LensError::InvalidInput {
                message: format!("Refusing unsafe restore path {:?}", receipt.original_path),
            });
        }

        if check_identity {
            // Verify the file in trash is still the file that was trashed.
            let current =
                fs::symlink_metadata(&receipt.trashed_file_path).map_err(|e| LensError::Io {
                    path: receipt.trashed_file_path.clone(),
                    source: e,
                })?;
            if !identity_matches(&FileIdentity::from_metadata(&current), &receipt.identity) {
                return Err(LensError::InvalidInput {
                    message: format!(
                        "Trashed file {:?} no longer matches its recorded identity",
                        receipt.trashed_file_path
                    ),
                });
            }
        } else if !occupied(&receipt.trashed_file_path) {
            return Err(LensError::InvalidInput {
                message: format!(
                    "Trash entry {:?} has no file to restore",
                    receipt.trashed_file_path
                ),
            });
        }

        if occupied(&receipt.original_path) {
            return Err(LensError::InvalidInput {
                message: format!(
                    "Destination {:?} already exists; refusing to overwrite",
                    receipt.original_path
                ),
            });
        }

        if let Some(parent) = receipt.original_path.parent() {
            fs::create_dir_all(parent).map_err(|e| LensError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        fs::rename(&receipt.trashed_file_path, &receipt.original_path).map_err(|e| {
            LensError::Io {
                path: receipt.trashed_file_path.clone(),
                source: e,
            }
        })?;

        // Remove the .trashinfo and the identity sidecar.
        let _ = fs::remove_file(&receipt.info_path);
        if let Some(name) = receipt
            .info_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".trashinfo"))
        {
            let _ = fs::remove_file(self.identity_path(name));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_trash(tag: &str) -> (tempfile::TempDir, TrashManager) {
        let root = tempfile::Builder::new()
            .prefix(&format!("lensdisk-trash-{tag}-"))
            .tempdir()
            .unwrap();
        let trash_dir = root.path().join("Trash");
        (root, TrashManager::new(trash_dir))
    }

    #[test]
    fn test_trash_list_and_restore_by_name() {
        let (root, trash) = tmp_trash("roundtrip");
        let src_dir = root.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let f1 = src_dir.join("one.txt");
        let f2 = src_dir.join("two.txt");
        fs::write(&f1, "111").unwrap();
        fs::write(&f2, "222").unwrap();

        trash.move_to_trash(&f1).unwrap();
        trash.move_to_trash(&f2).unwrap();
        assert!(!f1.exists() && !f2.exists());

        let entries = trash.list().unwrap();
        assert_eq!(entries.len(), 2);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"one.txt"));
        assert!(names.contains(&"two.txt"));
        let e = entries.iter().find(|e| e.name == "one.txt").unwrap();
        assert!(e.original_path.ends_with("one.txt"));
        assert!(e.present && e.has_identity);

        // Restore by name: file returns, receipts are removed.
        trash.restore_by_name("one.txt").unwrap();
        assert!(f1.exists());
        assert_eq!(fs::read_to_string(&f1).unwrap(), "111");
        assert_eq!(trash.list().unwrap().len(), 1);

        // Restore refuses when the destination is occupied.
        fs::write(&f2, "occupied").unwrap();
        assert!(trash.restore_by_name("two.txt").is_err());
        assert_eq!(fs::read_to_string(&f2).unwrap(), "occupied");

        // Traversal / unsafe names fail closed.
        assert!(trash.restore_by_name("../escape").is_err());
        assert!(trash.restore_by_name("").is_err());
        assert!(trash.restore_by_name(".").is_err());
        assert!(trash.restore_by_name("..").is_err());
        assert!(trash.restore_by_name("bad\0name").is_err());
    }

    #[test]
    fn test_trash_dotfile_and_info_suffix_roundtrip() {
        let (root, trash) = tmp_trash("dotfile");
        let src_dir = root.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let dot_env = src_dir.join(".env");
        let report_info = src_dir.join("report.info");
        fs::write(&dot_env, "SECRET=1").unwrap();
        fs::write(&report_info, "info-body").unwrap();

        trash.move_to_trash(&dot_env).unwrap();
        trash.move_to_trash(&report_info).unwrap();
        assert!(!dot_env.exists() && !report_info.exists());

        let entries = trash.list().unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&".env"), "{names:?}");
        assert!(names.contains(&"report.info"), "{names:?}");

        trash.restore_by_name(".env").unwrap();
        trash.restore_by_name("report.info").unwrap();
        assert_eq!(fs::read_to_string(&dot_env).unwrap(), "SECRET=1");
        assert_eq!(fs::read_to_string(&report_info).unwrap(), "info-body");
        assert!(trash.list().unwrap().is_empty());
        assert!(!trash.identity_path("report.info").exists());
    }

    #[test]
    fn test_restore_refuses_tampered_trash_file() {
        let (root, trash) = tmp_trash("tamper");
        let src = root.path().join("victim.txt");
        fs::write(&src, "orig").unwrap();
        trash.move_to_trash(&src).unwrap();

        // Swap the trashed file's content — the identity sidecar must
        // catch it and refuse restore.
        let trashed = trash.files_dir().join("victim.txt");
        fs::write(&trashed, "tampered!").unwrap();
        assert!(trash.restore_by_name("victim.txt").is_err());
    }
}
