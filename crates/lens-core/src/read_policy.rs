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
    match std::fs::canonicalize(path) {
        Ok(canonical) => {
            validate_resolved(path, &canonical, roots)?;
            Ok(canonical)
        }
        Err(error) => {
            // A dangling link must not turn a denied target into a different
            // response and become an existence oracle. Check its nearest
            // resolvable ancestor, following dangling link targets as well.
            check_ancestor(path, roots, 0, path)?;
            Err(error)
        }
    }
}

fn validate_resolved(path: &Path, resolved: &Path, roots: &[PathBuf]) -> std::io::Result<()> {
    if roots.iter().any(|root| resolved.starts_with(root)) {
        return Ok(());
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

fn check_ancestor(
    path: &Path,
    roots: &[PathBuf],
    depth: usize,
    origin: &Path,
) -> std::io::Result<()> {
    if depth >= 40 {
        return Err(std::io::Error::other("too many symbolic links"));
    }
    let mut candidate = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&candidate) {
            return validate_resolved(origin, &canonical, roots);
        }
        if let Ok(target) = std::fs::read_link(&candidate) {
            let target = if target.is_absolute() {
                target
            } else {
                candidate.parent().unwrap_or(Path::new(".")).join(target)
            };
            return check_ancestor(&target, roots, depth + 1, origin);
        }
        if !candidate.pop() {
            return Err(std::io::Error::other("cannot resolve read boundary"));
        }
    }
}

/// Resolve a metadata/alias target using the same boundary as content reads.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    if ROOTS.get().is_some_and(|roots| !roots.is_empty()) {
        checked(path.as_ref())
    } else {
        std::fs::canonicalize(path)
    }
}

pub fn metadata(path: impl AsRef<Path>) -> std::io::Result<std::fs::Metadata> {
    std::fs::metadata(checked(path.as_ref())?)
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
