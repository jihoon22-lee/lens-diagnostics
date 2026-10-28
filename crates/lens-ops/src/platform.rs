//! Live OS identity is kept separate from supplied artifacts and procfs fixtures.
use lens_core::{LensError, Result};

pub fn current_user_manager() -> Result<lens_sys::ManagerContext> {
    #[cfg(unix)]
    {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
            let mut result = std::ptr::null_mut();
            // Reentrant lookup: all returned strings live in buffer until copied.
            let rc = unsafe {
                libc::getpwuid_r(
                    libc::geteuid(),
                    &mut entry,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                )
            };
            if rc == libc::ERANGE && buffer.len() < 1024 * 1024 {
                buffer.resize(buffer.len() * 2, 0);
                continue;
            }
            if rc != 0 || result.is_null() || entry.pw_name.is_null() || entry.pw_dir.is_null() {
                return Err(LensError::Usage {
                    message: "cannot resolve the current manager account".into(),
                });
            }
            let user = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
                .to_string_lossy()
                .into_owned();
            let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) }
                .to_string_lossy()
                .into_owned();
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .filter(|s| std::path::Path::new(s).is_absolute())
                .unwrap_or_else(|| format!("/run/user/{}", unsafe { libc::geteuid() }));
            return Ok(lens_sys::ManagerContext {
                user,
                home,
                runtime_dir,
            });
        }
    }
    #[cfg(not(unix))]
    Err(LensError::Usage {
        message: "user manager inspection requires Unix account identity".into(),
    })
}

/// XDG paths are configuration inputs, never a substitute for account identity.
pub fn user_unit_paths(manager: &lens_sys::ManagerContext) -> Vec<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let configured = |name: &str, fallback: PathBuf| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or(fallback)
    };
    let config = configured("XDG_CONFIG_HOME", Path::new(&manager.home).join(".config"));
    let data = configured(
        "XDG_DATA_HOME",
        Path::new(&manager.home).join(".local/share"),
    );
    let runtime = Path::new(&manager.runtime_dir);
    let mut dirs = vec![
        config.join("systemd/user.control"),
        runtime.join("systemd/user.control"),
        runtime.join("systemd/transient"),
        runtime.join("systemd/generator.early"),
        config.join("systemd/user"),
    ];
    let list = |name: &str, fallback: &str| {
        std::env::var(name)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback.into())
            .split(':')
            .filter(|s| Path::new(s).is_absolute())
            .map(|s| Path::new(s).join("systemd/user"))
            .collect::<Vec<_>>()
    };
    dirs.extend(list("XDG_CONFIG_DIRS", "/etc/xdg"));
    dirs.extend([
        PathBuf::from("/etc/systemd/user"),
        runtime.join("systemd/user"),
        PathBuf::from("/run/systemd/user"),
        runtime.join("systemd/generator"),
        data.join("systemd/user"),
    ]);
    dirs.extend(list("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    dirs.extend([
        PathBuf::from("/usr/local/lib/systemd/user"),
        PathBuf::from("/usr/lib/systemd/user"),
        runtime.join("systemd/generator.late"),
    ]);
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|p| seen.insert(p.clone()));
    dirs
}
