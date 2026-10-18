use crate::model::{PyPackage, ShadowingIssue};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// CPython `sys.stdlib_module_names` (3.11) — the full list matters
/// because partially covering it produces false negatives (`secrets.py`,
/// `test.py`). Builtins and platform-specific entries are included; a
/// local file shadowing a module that does not exist on this platform is
/// still worth flagging.
const STDLIB_MODULES: &[&str] = &[
    "abc",
    "aifc",
    "argparse",
    "array",
    "ast",
    "asynchat",
    "asyncio",
    "asyncore",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "calendar",
    "cgi",
    "cgitb",
    "chunk",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "cProfile",
    "crypt",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "distutils",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "imghdr",
    "imp",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "lib2to3",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "mailcap",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "nis",
    "nntplib",
    "numbers",
    "operator",
    "optparse",
    "os",
    "ossaudiodev",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pipes",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtpd",
    "smtplib",
    "sndhdr",
    "socket",
    "socketserver",
    "spwd",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "sunau",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "test",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

/// A name usable as a top-level `import` target: `foo.py` or a `foo/`
/// package directory directly inside the project root or `src/`.
/// Nested files like `pkg/json.py` import as `pkg.json`, not `json`,
/// so they must not be flagged — only depth-1 names shadow.
fn candidate_modules(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = lens_core::read_policy::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_file() {
            if let Some(stem) = name.strip_suffix(".py") {
                if stem != "__init__" {
                    out.push((stem.to_string(), path.clone()));
                }
            }
        } else if path.is_dir() && path.join("__init__.py").is_file() {
            out.push((name.to_string(), path.join("__init__.py")));
        }
    }
    out
}

pub fn detect_shadowing(
    project_root: &Path,
    installed_packages: &BTreeMap<String, PyPackage>,
) -> Vec<ShadowingIssue> {
    let mut issues = Vec::new();
    let stdlib_set: HashSet<&str> = STDLIB_MODULES.iter().copied().collect();
    let self_packages = collect_self_packages(project_root, installed_packages);

    // Dist name → importable module names (`PyYAML` → `yaml`). Prefer the
    // declared top-level modules; fall back to the normalized dist name.
    let mut installed_modules: BTreeMap<String, String> = BTreeMap::new();
    for pkg in installed_packages.values() {
        let norm_name = crate::metadata::normalize_package_name(&pkg.name);
        if self_packages.contains(&norm_name) {
            continue;
        }
        if pkg.top_level_modules.is_empty() {
            installed_modules.insert(pkg.name.replace('-', "_").to_lowercase(), pkg.name.clone());
        } else {
            for m in &pkg.top_level_modules {
                installed_modules.insert(m.clone(), pkg.name.clone());
            }
        }
    }

    let scan_dirs = [project_root.to_path_buf(), project_root.join("src")];

    for dir in &scan_dirs {
        if !dir.exists() {
            continue;
        }

        for (mod_name, path) in candidate_modules(dir) {
            if stdlib_set.contains(mod_name.as_str()) {
                issues.push(ShadowingIssue {
                    module_name: mod_name.clone(),
                    local_path: path.to_string_lossy().to_string(),
                    shadows: format!("Python standard library module '{}'", mod_name),
                });
            } else if let Some(dist) = installed_modules.get(&mod_name) {
                issues.push(ShadowingIssue {
                    module_name: mod_name,
                    local_path: path.to_string_lossy().to_string(),
                    shadows: format!("Installed third-party package '{}'", dist),
                });
            }
        }
    }

    issues.sort_by(|a, b| {
        a.module_name
            .cmp(&b.module_name)
            .then(a.local_path.cmp(&b.local_path))
    });
    issues.dedup();
    issues
}

/// Process-local registry mapping canonical project root paths to normalized
/// package names installed from that root via `direct_url.json` or `.pth`.
fn editable_registry(
) -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, HashSet<String>>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, HashSet<String>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn normalize_dir_key(path: &Path) -> std::path::PathBuf {
    lens_core::read_policy::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn register_editable_root(target_dir: &Path, norm_pkg_name: &str) {
    let key = normalize_dir_key(target_dir);
    if let Ok(mut map) = editable_registry().lock() {
        map.entry(key.clone())
            .or_default()
            .insert(norm_pkg_name.to_string());
        // If `.pth` points at `<project>/src`, also associate `<project>`.
        if key.file_name().and_then(|n| n.to_str()) == Some("src") {
            if let Some(parent) = key.parent() {
                map.entry(parent.to_path_buf())
                    .or_default()
                    .insert(norm_pkg_name.to_string());
            }
        }
    }
}

pub(crate) fn inspect_dist_info_editable(dist_info: &Path, norm_pkg_name: &str) {
    let direct_url_path = dist_info.join("direct_url.json");
    if let Ok(content) = lens_core::read_policy::read_to_string(&direct_url_path) {
        if let Some(target_dir) = parse_direct_url_target(&content) {
            register_editable_root(&target_dir, norm_pkg_name);
        }
    }
}

pub(crate) fn inspect_pth_editable(pth_path: &Path, packages: &BTreeMap<String, PyPackage>) {
    let Ok(content) = lens_core::read_policy::read_to_string(pth_path) else {
        return;
    };
    let stem = pth_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let stem_pkg = extract_pkg_from_pth_stem(stem);

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("import ") {
            continue;
        }
        let target = Path::new(line);
        if let Some(ref norm) = stem_pkg {
            register_editable_root(target, norm);
        }
        // Also match any package whose top-level modules live in `target`.
        let local_mods: HashSet<String> = candidate_modules(target)
            .into_iter()
            .map(|(m, _)| m)
            .collect();
        if !local_mods.is_empty() {
            for (norm, pkg) in packages {
                if !pkg.top_level_modules.is_empty()
                    && pkg.top_level_modules.iter().all(|m| local_mods.contains(m))
                {
                    register_editable_root(target, norm);
                }
            }
        }
    }
}

fn extract_pkg_from_pth_stem(stem: &str) -> Option<String> {
    let s = stem
        .strip_prefix("__editable__.")
        .or_else(|| stem.strip_prefix('_'))
        .unwrap_or(stem);
    let pkg_part = s.split('-').next().unwrap_or(s);
    if pkg_part.is_empty() {
        None
    } else {
        Some(crate::metadata::normalize_package_name(pkg_part))
    }
}

fn parse_direct_url_target(content: &str) -> Option<std::path::PathBuf> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let is_editable = v
        .get("dir_info")
        .and_then(|d| d.get("editable"))
        .and_then(|e| e.as_bool())
        .unwrap_or(false);
    if !is_editable {
        return None;
    }
    let url = v.get("url").and_then(|u| u.as_str())?;
    let path_str = url.strip_prefix("file://")?;
    // Handle `file://localhost/path` or `file:///path`
    let path_str = path_str.strip_prefix("localhost").unwrap_or(path_str);
    let decoded = path_str.replace("%20", " ");
    Some(std::path::PathBuf::from(decoded))
}

fn collect_self_packages(
    project_root: &Path,
    installed_packages: &BTreeMap<String, PyPackage>,
) -> HashSet<String> {
    let mut self_pkgs = HashSet::new();
    let root_key = normalize_dir_key(project_root);

    // 1. Check packages recorded during `inspect_venv` (`direct_url.json` / `.pth`).
    if let Ok(map) = editable_registry().lock() {
        if let Some(names) = map.get(&root_key) {
            self_pkgs.extend(names.iter().cloned());
        }
    }

    // 2. Check `pyproject.toml` and `setup.cfg` in `project_root`.
    if let Some(name) = read_pyproject_package_name(project_root) {
        self_pkgs.insert(crate::metadata::normalize_package_name(&name));
    }

    // 3. Check `pkg.dist_info` directly and common venv dirs inside `project_root`.
    for (norm, pkg) in installed_packages {
        if let Some(ref di) = pkg.dist_info {
            let di_path = Path::new(di);
            if di_path.is_dir() {
                if let Ok(content) =
                    lens_core::read_policy::read_to_string(di_path.join("direct_url.json"))
                {
                    if let Some(target) = parse_direct_url_target(&content) {
                        if normalize_dir_key(&target) == root_key {
                            self_pkgs.insert(norm.clone());
                        }
                    }
                }
            }
        }
    }

    for venv_name in [".venv", "venv", "env"] {
        let venv_dir = project_root.join(venv_name);
        if !venv_dir.is_dir() {
            continue;
        }
        let lib_dir = venv_dir.join("lib");
        let mut sp_dirs = vec![lib_dir.join("site-packages")];
        if let Ok(entries) = lens_core::read_policy::read_dir(&lib_dir) {
            for e in entries.flatten() {
                sp_dirs.push(e.path().join("site-packages"));
            }
        }
        for sp in sp_dirs {
            if !sp.is_dir() {
                continue;
            }
            for (norm, pkg) in installed_packages {
                if let Some(ref di) = pkg.dist_info {
                    let direct_url = sp.join(di).join("direct_url.json");
                    if let Ok(content) = lens_core::read_policy::read_to_string(&direct_url) {
                        if let Some(target) = parse_direct_url_target(&content) {
                            if normalize_dir_key(&target) == root_key {
                                self_pkgs.insert(norm.clone());
                            }
                        }
                    }
                }
            }
            if let Ok(entries) = lens_core::read_policy::read_dir(&sp) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("pth") {
                        inspect_pth_editable(&path, installed_packages);
                    }
                }
            }
        }
    }

    if let Ok(map) = editable_registry().lock() {
        if let Some(names) = map.get(&root_key) {
            self_pkgs.extend(names.iter().cloned());
        }
    }

    self_pkgs
}

fn read_pyproject_package_name(project_root: &Path) -> Option<String> {
    if let Ok(content) = lens_core::read_policy::read_to_string(project_root.join("pyproject.toml"))
    {
        let mut in_project_section = false;
        for line in content.lines() {
            let trimmed = line.split('#').next().unwrap_or("").trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_project_section = matches!(trimmed, "[project]" | "[tool.poetry]");
                continue;
            }
            if in_project_section {
                if let Some((k, v)) = trimmed.split_once('=') {
                    if k.trim() == "name" {
                        let v = v.trim().trim_matches(['"', '\'']);
                        if !v.is_empty() {
                            return Some(v.to_string());
                        }
                    }
                }
            }
        }
    }
    if let Ok(content) = lens_core::read_policy::read_to_string(project_root.join("setup.cfg")) {
        let mut in_metadata = false;
        for line in content.lines() {
            let trimmed = line.split('#').next().unwrap_or("").trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_metadata = trimmed == "[metadata]";
                continue;
            }
            if in_metadata {
                if let Some((k, v)) = trimmed.split_once('=') {
                    if k.trim() == "name" {
                        let v = v.trim();
                        if !v.is_empty() {
                            return Some(v.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_direct_url_target_requires_editable() {
        let editable_json = r#"{
            "url": "file:///path/to/my-pkg",
            "dir_info": {
                "editable": true
            }
        }"#;
        assert_eq!(
            parse_direct_url_target(editable_json),
            Some(std::path::PathBuf::from("/path/to/my-pkg"))
        );

        let non_editable_json = r#"{
            "url": "file:///path/to/my-pkg",
            "dir_info": {}
        }"#;
        assert_eq!(parse_direct_url_target(non_editable_json), None);

        let missing_dir_info = r#"{
            "url": "file:///path/to/my-pkg"
        }"#;
        assert_eq!(parse_direct_url_target(missing_dir_info), None);
    }
}
