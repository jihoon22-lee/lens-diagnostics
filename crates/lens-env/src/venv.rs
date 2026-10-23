use crate::markers::{eval_requirement, version_satisfies, MarkerEnv, ReqVerdict};
use crate::metadata::{normalize_package_name, parse_metadata};
use crate::model::{PyPackage, PyVenv};
use std::collections::BTreeMap;
use std::path::Path;

/// `extras` activates `extra == "name"` environment markers (PEP 508),
/// as `env check --extras name` would.
pub fn inspect_venv(venv_path: &Path, extras: &[String]) -> std::io::Result<PyVenv> {
    validate_venv(venv_path)?;

    let mut home = String::new();
    let mut python_version = String::new();

    let cfg_path = venv_path.join("pyvenv.cfg");
    if cfg_path.exists() {
        if let Ok(content) = lens_core::read_policy::read_to_string(&cfg_path) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("home =") {
                    home = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("version =") {
                    python_version = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("version_info =") {
                    // uv-created venvs record the interpreter version as
                    // `version_info` instead of `version`.
                    if python_version.is_empty() {
                        python_version = rest.trim().to_string();
                    }
                }
            }
        }
    }

    let mut packages: BTreeMap<String, PyPackage> = BTreeMap::new();

    // Look in lib/python*/site-packages
    let lib_dir = venv_path.join("lib");
    if lib_dir.exists() {
        if let Ok(entries) = lens_core::read_policy::read_dir(&lib_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let site_packages = p.join("site-packages");
                    if site_packages.exists() {
                        scan_site_packages(&site_packages, &mut packages);
                    }
                }
            }
        }
    }

    // Also look directly in lib/site-packages (e.g. Windows or flat venvs)
    let direct_site_packages = lib_dir.join("site-packages");
    if direct_site_packages.exists() {
        scan_site_packages(&direct_site_packages, &mut packages);
    }

    // Check declared dependencies: PEP 508 markers decide whether a
    // requirement applies at all, and version specifiers are checked
    // against installed versions rather than mere presence.
    let marker_env = MarkerEnv::for_venv(&python_version, extras);
    let mut missing_dependencies = Vec::new();
    let mut version_conflicts = Vec::new();
    let mut unevaluated_dependencies = Vec::new();
    for pkg in packages.values() {
        for req in &pkg.requires_dist {
            match eval_requirement(req, &marker_env) {
                ReqVerdict::Inactive => {}
                ReqVerdict::Unevaluated => {
                    unevaluated_dependencies.push(format!(
                        "Package '{}' has a requirement that could not be evaluated: '{}'",
                        pkg.name,
                        req.trim()
                    ));
                }
                ReqVerdict::Active { name, spec } => {
                    let norm = normalize_package_name(&name);
                    match packages.get(&norm) {
                        None => missing_dependencies.push(format!(
                            "Package '{}' requires '{}' (not installed)",
                            pkg.name,
                            req.trim()
                        )),
                        Some(installed) => match version_satisfies(&installed.version, &spec) {
                            Some(true) => {}
                            Some(false) => version_conflicts.push(format!(
                                "Package '{}' requires '{}' but {} {} is installed",
                                pkg.name,
                                req.trim(),
                                installed.name,
                                installed.version
                            )),
                            None => unevaluated_dependencies.push(format!(
                                "Package '{}' has a requirement that could not be evaluated: '{}'",
                                pkg.name,
                                req.trim()
                            )),
                        },
                    }
                }
            }
        }
    }
    missing_dependencies.sort();
    missing_dependencies.dedup();
    version_conflicts.sort();
    version_conflicts.dedup();
    unevaluated_dependencies.sort();
    unevaluated_dependencies.dedup();

    Ok(PyVenv {
        path: venv_path.to_string_lossy().to_string(),
        python_version,
        home,
        packages,
        missing_dependencies,
        version_conflicts,
        unevaluated_dependencies,
        shadowing_issues: Vec::new(),
    })
}

/// A path that is not a virtualenv must not yield an empty "all satisfied"
/// report — fail closed on missing `pyvenv.cfg` and `site-packages`.
fn validate_venv(venv_path: &Path) -> std::io::Result<()> {
    if venv_path.join("pyvenv.cfg").is_file() {
        return Ok(());
    }
    let lib = venv_path.join("lib");
    let has_site_packages = lib.join("site-packages").is_dir()
        || lens_core::read_policy::read_dir(&lib)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| e.path().join("site-packages").is_dir())
            })
            .unwrap_or(false);
    if has_site_packages {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "not a Python virtualenv (no pyvenv.cfg or site-packages): {}",
            venv_path.display()
        ),
    ))
}

fn scan_site_packages(site_packages: &Path, packages: &mut BTreeMap<String, PyPackage>) {
    let mut pth_files = Vec::new();
    if let Ok(entries) = lens_core::read_policy::read_dir(site_packages) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                    if dir_name.ends_with(".dist-info") {
                        let meta_path = path.join("METADATA");
                        if meta_path.exists() {
                            if let Ok(content) = lens_core::read_policy::read_to_string(&meta_path)
                            {
                                if let Some(mut pkg) = parse_metadata(&content, Some(dir_name)) {
                                    pkg.top_level_modules =
                                        read_top_level_modules(&path, &pkg.name);
                                    let norm = normalize_package_name(&pkg.name);
                                    crate::shadowing::inspect_dist_info_editable(&path, &norm);
                                    packages.insert(norm, pkg);
                                }
                            }
                        }
                    }
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("pth") {
                pth_files.push(path);
            }
        }
    }
    for pth in pth_files {
        crate::shadowing::inspect_pth_editable(&pth, packages);
    }
}

/// Importable top-level module names for a distribution. `top_level.txt`
/// is authoritative when present; otherwise the top-level path components
/// in `RECORD` identify installed modules/packages. Falls back to the
/// normalized dist name with separators mapped to `_` (e.g.
/// `typing-extensions` → `typing_extensions`), which is how pip maps the
/// common cases.
fn read_top_level_modules(dist_info: &Path, dist_name: &str) -> Vec<String> {
    if let Ok(content) = lens_core::read_policy::read_to_string(dist_info.join("top_level.txt")) {
        let mut mods: Vec<String> = content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && *l != "__pycache__")
            .map(String::from)
            .collect();
        if !mods.is_empty() {
            mods.sort();
            mods.dedup();
            return mods;
        }
    }
    if let Ok(content) = lens_core::read_policy::read_to_string(dist_info.join("RECORD")) {
        let mut mods = std::collections::BTreeSet::new();
        for line in content.lines() {
            let path_field = line.split(',').next().unwrap_or("");
            let top = match path_field.split('/').next() {
                Some(t) if !t.is_empty() && !t.starts_with('.') => t,
                _ => continue,
            };
            if top == "__pycache__" || top.ends_with(".dist-info") || top.ends_with(".data") {
                continue;
            }
            if let Some(stem) = top.strip_suffix(".py") {
                if stem != "__init__" && stem != "__pycache__" {
                    mods.insert(stem.to_string());
                }
            } else if top.ends_with(".so") || top.ends_with(".pyd") {
                if let Some(stem) = top.split('.').next() {
                    if !stem.is_empty() && stem != "__init__" && stem != "__pycache__" {
                        mods.insert(stem.to_string());
                    }
                }
            } else if !top.contains('.') {
                mods.insert(top.to_string());
            }
        }
        if !mods.is_empty() {
            return mods.into_iter().collect();
        }
    }
    vec![dist_name.replace('-', "_").to_lowercase()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn installed_development_release_does_not_satisfy_exact_rc_requirement() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("pyvenv.cfg"), "version = 3.11.0\n").unwrap();
        let site = root.path().join("lib/python3.11/site-packages");
        for (name, version, extra) in [
            ("app", "1.0", "Requires-Dist: dep ==1.0rc1\n"),
            ("dep", "1.0rc1.dev1", ""),
        ] {
            let info = site.join(format!("{name}-{version}.dist-info"));
            fs::create_dir_all(&info).unwrap();
            fs::write(
                info.join("METADATA"),
                format!("Name: {name}\nVersion: {version}\n{extra}"),
            )
            .unwrap();
        }
        let venv = inspect_venv(root.path(), &[]).unwrap();
        assert_eq!(venv.version_conflicts.len(), 1);
        assert!(venv.version_conflicts[0].contains("1.0rc1.dev1"));
        assert!(venv.unevaluated_dependencies.is_empty());
    }

    /// Build a minimal venv fixture: pyvenv.cfg + one dist-info per
    /// (name, version, requires) tuple.
    fn make_venv(cfg: &str, dists: &[(&str, &str, &[&str])]) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("pyvenv.cfg"), cfg).unwrap();
        let sp = root.join("lib/python3.14/site-packages");
        fs::create_dir_all(&sp).unwrap();
        for (name, version, requires) in dists {
            let di = sp.join(format!("{}-{}.dist-info", name, version));
            fs::create_dir_all(&di).unwrap();
            let mut meta = format!(
                "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
                name, version
            );
            for r in *requires {
                meta.push_str(&format!("Requires-Dist: {}\n", r));
            }
            fs::write(di.join("METADATA"), meta).unwrap();
        }
        dir
    }

    #[test]
    fn test_uv_version_info_populates_python_version() {
        let dir = make_venv(
            "uv = 0.9.0\nhome = /usr/bin\nimplementation = cpython\nversion_info = 3.14.4\n",
            &[],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert_eq!(venv.python_version, "3.14.4");
    }

    #[test]
    fn test_markers_skip_inapplicable_requirements() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[(
                "flask",
                "3.1.0",
                &[
                    "importlib-metadata>=3.6.0; python_version < '3.10'",
                    "pytest; extra == 'testing'",
                    "asgiref>=3.2 ; extra == \"async\"",
                ],
            )],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert!(venv.missing_dependencies.is_empty());
        assert!(venv.version_conflicts.is_empty());
        assert!(venv.unevaluated_dependencies.is_empty());
    }

    #[test]
    fn test_extras_flag_activates_extra_markers() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[("app", "1.0", &["pytest>=9.0; extra == 'testing'"])],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert!(venv.missing_dependencies.is_empty());
        let venv = inspect_venv(dir.path(), &["testing".to_string()]).unwrap();
        assert_eq!(venv.missing_dependencies.len(), 1);
        assert!(venv.missing_dependencies[0].contains("pytest"));
    }

    #[test]
    fn test_version_conflict_reported_not_missing() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[("app", "1.0", &["werkzeug<3"]), ("werkzeug", "3.1.9", &[])],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert!(venv.missing_dependencies.is_empty());
        assert_eq!(venv.version_conflicts.len(), 1);
        assert!(venv.version_conflicts[0].contains("werkzeug<3"));
        assert!(venv.version_conflicts[0].contains("3.1.9"));
    }

    #[test]
    fn test_unevaluated_marker_is_not_missing() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[("app", "1.0", &["esoteric-pkg; platform_release > '9'"])],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert!(venv.missing_dependencies.is_empty());
        assert_eq!(venv.unevaluated_dependencies.len(), 1);
        assert!(venv.unevaluated_dependencies[0].contains("esoteric-pkg"));
    }

    #[test]
    fn test_satisfied_requirement_is_quiet() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[
                ("app", "1.0", &["werkzeug<4,>=3.1; sys_platform == 'linux'"]),
                ("werkzeug", "3.1.9", &[]),
            ],
        );
        let venv = inspect_venv(dir.path(), &[]).unwrap();
        assert!(venv.missing_dependencies.is_empty());
        assert!(venv.version_conflicts.is_empty());
        assert!(venv.unevaluated_dependencies.is_empty());
    }

    #[test]
    fn test_record_excludes_pycache_and_extracts_c_extensions() {
        let dir = make_venv(
            "home = /usr/bin\nversion = 3.14.4\n",
            &[("ujson", "5.9.0", &[])],
        );
        let di = dir
            .path()
            .join("lib/python3.14/site-packages/ujson-5.9.0.dist-info");
        fs::write(
            di.join("RECORD"),
            "__pycache__/helper.cpython-314.pyc,,\nujson.cpython-314-x86_64-linux-gnu.so,,\nmyext.abi3.so,,\nwinmod.cp314-win_amd64.pyd,,\nujson-5.9.0.dist-info/METADATA,,\n",
        )
        .unwrap();

        let venv = inspect_venv(dir.path(), &[]).unwrap();
        let pkg = &venv.packages["ujson"];
        assert_eq!(
            pkg.top_level_modules,
            vec![
                "myext".to_string(),
                "ujson".to_string(),
                "winmod".to_string()
            ]
        );
    }

    #[test]
    fn test_editable_self_install_not_flagged_as_shadowing() {
        let dir = tempdir().unwrap();
        let proj = dir.path().join("myproj");
        fs::create_dir_all(proj.join("src/myproj")).unwrap();
        fs::write(proj.join("src/myproj/__init__.py"), "").unwrap();
        fs::write(
            proj.join("pyproject.toml"),
            "[project]\nname = \"my-proj\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let venv_dir = proj.join(".venv");
        let sp = venv_dir.join("lib/python3.14/site-packages");
        let di = sp.join("my_proj-0.1.0.dist-info");
        fs::create_dir_all(&di).unwrap();
        fs::write(
            venv_dir.join("pyvenv.cfg"),
            "home = /usr/bin\nversion = 3.14.4\n",
        )
        .unwrap();
        fs::write(
            di.join("METADATA"),
            "Metadata-Version: 2.1\nName: my-proj\nVersion: 0.1.0\n",
        )
        .unwrap();
        fs::write(di.join("top_level.txt"), "myproj\n").unwrap();
        fs::write(
            di.join("direct_url.json"),
            format!(
                "{{\"dir_info\": {{\"editable\": true}}, \"url\": \"file://{}\"}}",
                proj.display()
            ),
        )
        .unwrap();

        let venv = inspect_venv(&venv_dir, &[]).unwrap();
        let issues = crate::shadowing::detect_shadowing(&proj, &venv.packages);
        assert!(
            issues.is_empty(),
            "editable self-package must not shadow itself: {issues:?}"
        );
    }
}
