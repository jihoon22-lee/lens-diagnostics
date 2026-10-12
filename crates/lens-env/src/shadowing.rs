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
    // Dist name → importable module names (`PyYAML` → `yaml`). Prefer the
    // declared top-level modules; fall back to the normalized dist name.
    let mut installed_modules: BTreeMap<String, String> = BTreeMap::new();
    for pkg in installed_packages.values() {
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
