use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::{LensError, Result};

/// POSIX file system identity to detect file modifications and hard link sharing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub size: u64,
    pub modified_sec: i64,
    pub modified_nsec: i64,
    pub changed_sec: i64,
    pub changed_nsec: i64,
}

impl FileIdentity {
    pub fn from_metadata(meta: &std::fs::Metadata) -> Self {
        crate::platform::file_identity(meta)
    }

    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let meta = std::fs::symlink_metadata(path.as_ref()).map_err(|e| LensError::Io {
            path: path.as_ref().to_path_buf(),
            source: e,
        })?;
        Ok(Self::from_metadata(&meta))
    }

    /// Identity of the file the path resolves to (follows symlinks), matching
    /// the semantics of `File::open` used by `SafeInput`.
    pub fn from_path_follow<P: AsRef<Path>>(path: P) -> Result<Self> {
        let meta = std::fs::metadata(path.as_ref()).map_err(|e| LensError::Io {
            path: path.as_ref().to_path_buf(),
            source: e,
        })?;
        Ok(Self::from_metadata(&meta))
    }

    pub fn from_file(file: &File, path: &Path) -> Result<Self> {
        let meta = file.metadata().map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Self::from_metadata(&meta))
    }

    pub fn is_same_file(&self, other: &Self) -> bool {
        (self.device != 0 || self.inode != 0)
            && self.device == other.device
            && self.inode == other.inode
    }

    pub fn is_unchanged(&self, current: &Self) -> bool {
        self == current
    }
}

/// A securely opened file with identity tracking against TOCTOU race conditions.
#[derive(Debug)]
pub struct SafeInput {
    pub path: PathBuf,
    pub file: File,
    pub initial_identity: FileIdentity,
}

impl SafeInput {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        // Check read policy and verify regular-file mode via stat before open(2)
        // so that opening a FIFO (named pipe) or device node never blocks.
        let pre_meta = crate::read_policy::metadata(&path_buf).map_err(|e| LensError::Io {
            path: path_buf.clone(),
            source: e,
        })?;
        let pre_identity = FileIdentity::from_metadata(&pre_meta);
        if (pre_identity.mode & 0o170000) != 0o100000 {
            return Err(LensError::InvalidInput {
                message: format!("Target {:?} is not a regular file", path_buf),
            });
        }

        let file = crate::platform::open_nonblocking(&path_buf).map_err(|e| LensError::Io {
            path: path_buf.clone(),
            source: e,
        })?;
        let initial_identity = FileIdentity::from_file(&file, &path_buf)?;

        // Verify it is still a regular file and matches pre-open identity
        if (initial_identity.mode & 0o170000) != 0o100000 {
            return Err(LensError::InvalidInput {
                message: format!("Target {:?} is not a regular file", path_buf),
            });
        }
        if !pre_identity.is_same_file(&initial_identity) {
            return Err(LensError::InputChanged { path: path_buf });
        }

        Ok(Self {
            path: path_buf,
            file,
            initial_identity,
        })
    }

    /// Verifies that the file content and metadata on disk have not been altered.
    pub fn verify_unchanged(&self) -> Result<()> {
        let current_file_meta = FileIdentity::from_file(&self.file, &self.path)?;
        if !self.initial_identity.is_unchanged(&current_file_meta) {
            return Err(LensError::InputChanged {
                path: self.path.clone(),
            });
        }
        // `from_path` (lstat) sees the symlink inode, which never matches the
        // opened file's identity; compare against the resolved target instead.
        let current_path_meta = FileIdentity::from_path_follow(&self.path)?;
        if !self.initial_identity.is_unchanged(&current_path_meta) {
            return Err(LensError::InputChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Safely maps the file to memory.
    pub fn mmap(&self) -> Result<memmap2::Mmap> {
        self.verify_unchanged()?;
        // SAFETY: The file is verified to be a regular file, opened read-only, and unchanged.
        unsafe {
            memmap2::MmapOptions::new()
                .map(&self.file)
                .map_err(|e| LensError::Io {
                    path: self.path.clone(),
                    source: e,
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_zero_file_identity_is_not_same_file() {
        let a = FileIdentity::default();
        let b = FileIdentity::default();
        assert!(!a.is_same_file(&b));
    }

    #[test]
    fn safe_input_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = SafeInput::open(dir.path()).unwrap_err();
        assert!(matches!(err, LensError::InvalidInput { .. }));
    }
}
