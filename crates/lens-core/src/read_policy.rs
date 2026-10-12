//! Optional process-wide read boundary for the MCP server. CLI/library users
//! have no policy by default. Check discovered paths at the point of I/O, not
//! only the directory supplied by the caller. Denials survive callers that
//! normally recover from missing or unreadable evidence.
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
static DENIAL: Mutex<Option<String>> = Mutex::new(None);

pub fn set_roots(roots: Vec<PathBuf>) -> Result<(), &'static str> {
    ROOTS
        .set(roots)
        .map_err(|_| "read roots already configured")
}

pub fn take_denial() -> Option<String> {
    DENIAL.lock().unwrap().take()
}

fn checked(path: &Path) -> std::io::Result<PathBuf> {
    let Some(roots) = ROOTS.get().filter(|r| !r.is_empty()) else {
        return Ok(path.to_path_buf());
    };
    let canonical = std::fs::canonicalize(path)?;
    if roots.iter().any(|root| canonical.starts_with(root)) {
        return Ok(canonical);
    }
    let message = format!("path {} is outside the allowed roots", path.display());
    DENIAL
        .lock()
        .unwrap()
        .get_or_insert_with(|| message.clone());
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        message,
    ))
}

pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    std::fs::read(checked(path.as_ref())?)
}

pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    std::fs::read_to_string(checked(path.as_ref())?)
}

pub fn read_dir(path: impl AsRef<Path>) -> std::io::Result<std::fs::ReadDir> {
    // Preserve lexical entry names (systemd aliases/drop-ins depend on them).
    checked(path.as_ref())?;
    std::fs::read_dir(path)
}
