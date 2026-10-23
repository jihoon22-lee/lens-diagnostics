//! Unit loading with `<unit>.d/*.conf` drop-in scanning.
//!
//! Mirrors systemd's on-disk lookup for the common local case: a unit file
//! `foo.service` is merged with every `foo.service.d/*.conf` sibling in
//! lexicographic filename order (later files override earlier ones).

use crate::model::{Diagnostic, SystemdUnit};
use crate::parser::{apply_drop_in, instantiate_template, parse_unit_content, template_name_of};
use lens_core::{LensError, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const UNIT_SUFFIXES: [&str; 10] = [
    ".service",
    ".socket",
    ".target",
    ".timer",
    ".mount",
    ".automount",
    ".swap",
    ".path",
    ".slice",
    ".scope",
];

fn unit_symlink_target(path: &Path) -> std::io::Result<PathBuf> {
    // A /dev/null mask is a fixed sentinel: classify it without opening it.
    if std::fs::read_link(path).is_ok_and(|target| target == Path::new("/dev/null")) {
        return Ok(PathBuf::from("/dev/null"));
    }
    lens_core::read_policy::canonicalize(path)
}

/// Load one unit file, then merge every `*.conf` in its `<name>.d/` directory.
pub fn load_unit(path: &Path) -> Result<SystemdUnit> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unit")
        .to_string();
    load_named_unit(path, &name, &[path.parent().unwrap_or(Path::new("."))])
}

fn load_named_unit(path: &Path, name: &str, dirs: &[impl AsRef<Path>]) -> Result<SystemdUnit> {
    let content = lens_core::read_policy::read_to_string(path).map_err(|source| LensError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut unit = parse_unit_content(&content, name, Some(&path.to_string_lossy()));
    apply_merged_drop_ins(&mut unit, dirs, name);
    Ok(unit)
}

fn resolve_loaded_instances(
    units: &mut BTreeMap<String, SystemdUnit>,
    dirs: &[impl AsRef<Path>],
) -> Result<()> {
    crate::resolve::expand_instances(units, |template, name| {
        let mut instance = if let Some(path) = &template.path {
            load_named_unit(Path::new(path), name, dirs)?
        } else {
            let mut instance = instantiate_template(template, name);
            apply_merged_drop_ins(&mut instance, dirs, name);
            instance
        };
        for dir in dirs.iter().rev() {
            apply_unit_dep_dirs(dir.as_ref(), &mut instance);
        }
        Ok(instance)
    })
}

/// Load every unit under `path` — either a single file or a directory of
/// unit files — each with its drop-ins applied.
pub fn load_units(path: &Path) -> Result<BTreeMap<String, SystemdUnit>> {
    let mut units = BTreeMap::new();
    if !path.exists() && !path.is_symlink() {
        // Fail closed: a misspelled --systemd-dir must not yield a vacuous
        // "0 units, no cycles" report.
        return Err(LensError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "unit path does not exist"),
        });
    }
    let paths = if path.is_symlink() && unit_name_of(path).is_some() || path.is_file() {
        vec![path.to_path_buf()]
    } else if path.is_dir() {
        let entries = lens_core::read_policy::read_dir(path).map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        // Unit-suffixed entries: regular files and symlinks both count.
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_type()
                    .map(|t| t.is_file() || t.is_symlink())
                    .unwrap_or(false)
                    && e.file_name()
                        .to_str()
                        .map(|n| UNIT_SUFFIXES.iter().any(|s| n.ends_with(s)))
                        .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        paths.sort();
        paths
    } else {
        Vec::new()
    };

    // (link name, canonical target unit name) for alias symlinks.
    let mut alias_pairs: Vec<(String, String)> = Vec::new();
    for p in paths {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        if p.is_symlink() {
            match unit_symlink_target(&p) {
                Ok(target) if target == Path::new("/dev/null") => {
                    // Masked unit: cannot be ordered, must not
                    // participate in the graph.
                    let mut unit = SystemdUnit {
                        name: name.clone(),
                        path: Some(p.to_string_lossy().into_owned()),
                        masked: true,
                        ..Default::default()
                    };
                    unit.diagnostics.push(Diagnostic {
                        code: "UNIT_MASKED".to_string(),
                        severity: "info".to_string(),
                        message: "unit is masked (symlink to /dev/null)".to_string(),
                        path: Some(p.to_string_lossy().into_owned()),
                        line: None,
                    });
                    units.insert(name, unit);
                    continue;
                }
                Ok(target) => {
                    let target_name = target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let is_template_instantiation = crate::parser::template_name_of(&name)
                        .as_deref()
                        == Some(target_name.as_str());
                    // A symlink whose basename differs from the link's
                    // is an alias (e.g. dbus-org.foo.service), unless it is
                    // an instance symlink pointing to its own template.
                    if !target_name.is_empty() && target_name != name && !is_template_instantiation
                    {
                        let mut unit = SystemdUnit {
                            name: name.clone(),
                            path: Some(p.to_string_lossy().into_owned()),
                            alias_of: Some(target_name.clone()),
                            ..Default::default()
                        };
                        unit.diagnostics.push(Diagnostic {
                            code: "UNIT_ALIAS".to_string(),
                            severity: "info".to_string(),
                            message: format!("alias for {}", target_name),
                            path: Some(p.to_string_lossy().into_owned()),
                            line: None,
                        });
                        units.insert(name.clone(), unit);
                        alias_pairs.push((name, target_name));
                        continue;
                    }
                }
                Err(e) => {
                    let mut unit = SystemdUnit {
                        name: name.clone(),
                        path: Some(p.to_string_lossy().into_owned()),
                        ..Default::default()
                    };
                    unit.diagnostics.push(Diagnostic {
                        code: "UNIT_UNREADABLE".to_string(),
                        severity: "error".to_string(),
                        message: format!("broken symlink: {}", e),
                        path: Some(p.to_string_lossy().into_owned()),
                        line: None,
                    });
                    units.insert(name, unit);
                    continue;
                }
            }
        }
        match load_unit(&p) {
            Ok(unit) => {
                units.insert(unit.name.clone(), unit);
            }
            Err(e) => {
                // Fail-closed: an unreadable unit is recorded rather than
                // silently skipped, so diagnostics surface the gap.
                let mut unit = SystemdUnit {
                    name: name.clone(),
                    path: Some(p.to_string_lossy().into_owned()),
                    ..Default::default()
                };
                unit.diagnostics.push(Diagnostic {
                    code: "UNIT_UNREADABLE".to_string(),
                    severity: "error".to_string(),
                    message: format!("{}", e),
                    path: Some(p.to_string_lossy().into_owned()),
                    line: None,
                });
                units.insert(name, unit);
            }
        }
    }
    if path.is_dir() {
        apply_dep_dirs(path, &mut units);
    }
    // Fold alias names onto their canonical units.
    for (alias_name, target_name) in alias_pairs {
        if let Some(target) = units.get_mut(&target_name) {
            target.aliases.push(alias_name);
        }
    }
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    resolve_loaded_instances(&mut units, &[dir])?;
    flag_missing_references(&mut units);
    Ok(units)
}

/// Dependency directives whose targets should resolve to real unit files.
const REF_KEYS: [&str; 4] = ["Wants", "Requires", "Before", "After"];

fn refs_for<'a>(unit: &'a SystemdUnit, directive: &str) -> &'a [String] {
    match directive {
        "Wants" => &unit.wants,
        "Requires" => &unit.requires,
        "Before" => &unit.before,
        _ => &unit.after,
    }
}

/// Suffixes for which an unresolved `Wants=`/`Requires=`/ordering target is
/// worth flagging. `.device`/`.mount`/`.automount`/`.swap`/`.scope`/`.slice`
/// units are commonly produced by generators or PID 1 itself, so missing
/// references to them are not diagnosed.
const REF_CHECKED_SUFFIXES: [&str; 5] = [".service", ".socket", ".target", ".timer", ".path"];

/// Record a `UNIT_REF_MISSING` warning on each unit that references a unit
/// name not present in the loaded set (aliases count; template references
/// like `foo@bar.service` resolve via `foo@.service`). Missing references
/// are almost always operator error — a typo or a dependency that was not
/// installed — so surfacing them keeps `sys inspect` honest.
fn flag_missing_references(units: &mut BTreeMap<String, SystemdUnit>) {
    let known: std::collections::BTreeSet<String> = units.keys().cloned().collect();
    let mut templates = std::collections::BTreeSet::new();
    for name in &known {
        if crate::parser::is_template_name(name) {
            templates.insert(name.clone());
        }
    }

    let unit_names: Vec<String> = units.keys().cloned().collect();
    for name in unit_names {
        let unit = units.get(&name).unwrap().clone();
        if unit.masked || unit.alias_of.is_some() || crate::parser::is_template_name(&name) {
            continue;
        }
        let mut missing: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        for directive in REF_KEYS {
            for target in refs_for(&unit, directive) {
                if !REF_CHECKED_SUFFIXES.iter().any(|s| target.ends_with(s)) {
                    continue;
                }
                if known.contains(target) {
                    continue;
                }
                // `foo@bar.service` resolves when template `foo@.service` exists.
                if let Some(at) = target.find('@') {
                    if let Some(dot) = target.rfind('.') {
                        if at < dot {
                            let tpl = format!("{}@{}", &target[..at], &target[dot..]);
                            if templates.contains(&tpl) {
                                continue;
                            }
                        }
                    }
                }
                missing.insert((directive.to_string(), target.clone()));
            }
        }
        let unit = units.get_mut(&name).unwrap();
        for (directive, target) in missing {
            unit.diagnostics.push(Diagnostic {
                code: "UNIT_REF_MISSING".to_string(),
                severity: "warning".to_string(),
                message: format!("{}={} does not match any loaded unit", directive, target),
                path: unit.path.clone(),
                line: None,
            });
        }
    }
}

/// Drop-in directories for a unit, in merge order. `foo@bar.service` gets
/// `foo@.service.d/` (template level) then `foo@bar.service.d/`.
fn drop_in_dirs(unit_path: &Path, unit_name: &str) -> Vec<PathBuf> {
    let parent = unit_path.parent().unwrap_or_else(|| Path::new("."));
    let mut dirs = Vec::new();
    if let Some(at) = unit_name.find('@') {
        if let Some(dot) = unit_name.rfind('.') {
            if at < dot {
                let template = format!("{}@{}.d", &unit_name[..at], &unit_name[dot..]);
                if template != format!("{unit_name}.d") {
                    dirs.push(parent.join(template));
                }
            }
        }
    }
    dirs.push(parent.join(format!("{}.d", unit_name)));
    dirs
}

/// systemd's unit search path in *decreasing* precedence order (later
/// entries are shadowed by earlier ones). `/lib/systemd/system` is the
/// historic location of what is now `/usr/lib/systemd/system` on merged-usr
/// systems; listing both keeps non-merged layouts working.
pub const SYSTEMD_SEARCH_DIRS: [&str; 4] = [
    "/etc/systemd/system",
    "/run/systemd/system",
    "/usr/lib/systemd/system",
    "/lib/systemd/system",
];

/// A unit file (or `.d` drop-in directory member) found in a search dir.
fn unit_name_of(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| UNIT_SUFFIXES.iter().any(|s| n.ends_with(s)))
        .map(|n| n.to_string())
}

/// Load all drop-in `*.conf` for `unit_name` from `dir` (template level
/// first, instance level last), returning them sorted by filename.
fn drop_in_confs(dir: &Path, unit_name: &str, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let unit_path = dir.join(unit_name);
    for drop_dir in drop_in_dirs(&unit_path, unit_name) {
        if !lens_core::read_policy::metadata(&drop_dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let mut confs: Vec<PathBuf> = match lens_core::read_policy::read_dir(&drop_dir) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "conf").unwrap_or(false))
                .collect(),
            Err(error) => {
                diagnostics.push(Diagnostic {
                    code: "DROPIN_UNREADABLE".to_string(),
                    severity: "warning".to_string(),
                    message: format!("cannot list {}: {error}", drop_dir.display()),
                    path: Some(drop_dir.to_string_lossy().into_owned()),
                    line: None,
                });
                Vec::new()
            }
        };
        confs.sort();
        out.extend(confs);
    }
    out
}

/// Merge-load units across systemd's search dirs (highest precedence first
/// in `dirs`). The unit *file* comes from the highest-precedence dir that
/// provides it; a mask (`/dev/null` symlink) or alias there shadows lower
/// dirs entirely. Drop-ins from **all** dirs still apply — lowest
/// precedence first so `/etc` drop-ins win over vendor defaults.
pub fn load_units_merged(dirs: &[impl AsRef<Path>]) -> Result<BTreeMap<String, SystemdUnit>> {
    let mut units = BTreeMap::new();

    // Collect candidate names per dir in precedence order.
    let dir_names: Vec<std::collections::BTreeSet<String>> = dirs
        .iter()
        .map(|d| {
            lens_core::read_policy::read_dir(d.as_ref())
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| {
                            e.file_type()
                                .map(|t| t.is_file() || t.is_symlink() || t.is_dir())
                                .unwrap_or(false)
                        })
                        .filter_map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            // `<name>.d` dirs contribute the base unit name.
                            n.strip_suffix(".d")
                                .filter(|b| UNIT_SUFFIXES.iter().any(|s| b.ends_with(s)))
                                .map(|b| b.to_string())
                                .or_else(|| unit_name_of(&e.path()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    let mut all_names = std::collections::BTreeSet::new();
    for set in &dir_names {
        all_names.extend(set.iter().cloned());
    }

    for name in all_names {
        // Highest-precedence dir that actually provides the unit file.
        let find_file = |name: &str| {
            dirs.iter().find_map(|dir| {
                let path = dir.as_ref().join(name);
                (path.is_file() || path.is_symlink()).then_some(path)
            })
        };
        let primary =
            find_file(&name).or_else(|| template_name_of(&name).and_then(|tpl| find_file(&tpl)));
        let Some(p) = primary else {
            // Only drop-ins exist — systemd synthesizes a stub unit.
            let mut unit = SystemdUnit {
                name: name.clone(),
                ..Default::default()
            };
            unit.diagnostics.push(Diagnostic {
                code: "UNIT_STUB".to_string(),
                severity: "info".to_string(),
                message: "unit defined only via drop-in overrides".to_string(),
                path: None,
                line: None,
            });
            apply_merged_drop_ins(&mut unit, dirs, &name);
            units.insert(name, unit);
            continue;
        };

        // Masked / alias / broken symlinks are handled at the winning dir —
        // a mask in /etc shadows the vendor unit entirely.
        if p.is_symlink() {
            match unit_symlink_target(&p) {
                Ok(target) if target == Path::new("/dev/null") => {
                    let mut unit = SystemdUnit {
                        name: name.clone(),
                        path: Some(p.to_string_lossy().into_owned()),
                        masked: true,
                        ..Default::default()
                    };
                    unit.diagnostics.push(Diagnostic {
                        code: "UNIT_MASKED".to_string(),
                        severity: "info".to_string(),
                        message: "unit is masked (symlink to /dev/null)".to_string(),
                        path: Some(p.to_string_lossy().into_owned()),
                        line: None,
                    });
                    units.insert(name, unit);
                    continue;
                }
                Ok(target) => {
                    let target_name = target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let is_template_instantiation = crate::parser::template_name_of(&name)
                        .as_deref()
                        == Some(target_name.as_str());
                    if !target_name.is_empty() && target_name != name && !is_template_instantiation
                    {
                        let mut unit = SystemdUnit {
                            name: name.clone(),
                            path: Some(p.to_string_lossy().into_owned()),
                            alias_of: Some(target_name.clone()),
                            ..Default::default()
                        };
                        unit.diagnostics.push(Diagnostic {
                            code: "UNIT_ALIAS".to_string(),
                            severity: "info".to_string(),
                            message: format!("alias for {}", target_name),
                            path: Some(p.to_string_lossy().into_owned()),
                            line: None,
                        });
                        units.insert(name, unit);
                        continue;
                    }
                }
                Err(e) => {
                    let mut unit = SystemdUnit {
                        name: name.clone(),
                        path: Some(p.to_string_lossy().into_owned()),
                        ..Default::default()
                    };
                    unit.diagnostics.push(Diagnostic {
                        code: "UNIT_UNREADABLE".to_string(),
                        severity: "error".to_string(),
                        message: format!("broken symlink: {}", e),
                        path: Some(p.to_string_lossy().into_owned()),
                        line: None,
                    });
                    units.insert(name, unit);
                    continue;
                }
            }
        }

        let mut unit = match lens_core::read_policy::read_to_string(&p) {
            Ok(content) => parse_unit_content(&content, &name, Some(&p.to_string_lossy())),
            Err(e) => {
                let mut unit = SystemdUnit {
                    name: name.clone(),
                    path: Some(p.to_string_lossy().into_owned()),
                    ..Default::default()
                };
                unit.diagnostics.push(Diagnostic {
                    code: "UNIT_UNREADABLE".to_string(),
                    severity: "error".to_string(),
                    message: format!("{}", e),
                    path: Some(p.to_string_lossy().into_owned()),
                    line: None,
                });
                unit
            }
        };
        apply_merged_drop_ins(&mut unit, dirs, &name);
        units.insert(name, unit);
    }

    for dir in dirs.iter().rev() {
        apply_dep_dirs(dir.as_ref(), &mut units);
    }

    // Fold alias names onto their canonical units.
    let alias_pairs: Vec<(String, String)> = units
        .iter()
        .filter_map(|(n, u)| u.alias_of.as_ref().map(|t| (n.clone(), t.clone())))
        .collect();
    for (alias_name, target_name) in alias_pairs {
        if let Some(target) = units.get_mut(&target_name) {
            target.aliases.push(alias_name);
        }
    }
    resolve_loaded_instances(&mut units, dirs)?;
    flag_missing_references(&mut units);
    Ok(units)
}

fn apply_dep_dirs(dir: &Path, units: &mut BTreeMap<String, SystemdUnit>) {
    for unit in units.values_mut() {
        apply_unit_dep_dirs(dir, unit);
    }
}

fn apply_unit_dep_dirs(dir: &Path, unit: &mut SystemdUnit) {
    let names: Vec<String> = template_name_of(&unit.name)
        .into_iter()
        .chain(std::iter::once(unit.name.clone()))
        .collect();
    for name in names {
        for (suffix, is_wants) in [(".wants", true), (".requires", false)] {
            let dep_dir = dir.join(format!("{name}{suffix}"));
            if !lens_core::read_policy::metadata(&dep_dir).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let Ok(rd) = lens_core::read_policy::read_dir(&dep_dir) else {
                continue;
            };
            let deps = rd.flatten().filter_map(|entry| unit_name_of(&entry.path()));
            let list = if is_wants {
                &mut unit.wants
            } else {
                &mut unit.requires
            };
            list.extend(deps);
            list.sort();
            list.dedup();
        }
    }
}

/// Apply drop-ins from every search dir, lowest precedence dir first so
/// the highest-precedence dir's conf wins per key.
fn apply_merged_drop_ins(unit: &mut SystemdUnit, dirs: &[impl AsRef<Path>], unit_name: &str) {
    // Same-name files override by search-path precedence; all surviving
    // files are then applied in filename order across directories.
    let mut chosen = BTreeMap::new();
    for dir in dirs.iter().rev() {
        for conf in drop_in_confs(dir.as_ref(), unit_name, &mut unit.diagnostics) {
            if let Some(name) = conf.file_name() {
                chosen.insert(name.to_os_string(), conf);
            }
        }
    }
    for conf in chosen.into_values() {
        if std::fs::read_link(&conf).is_ok_and(|target| target == Path::new("/dev/null")) {
            continue;
        }
        match lens_core::read_policy::read_to_string(&conf) {
            Ok(content) => apply_drop_in(unit, &content, &conf.to_string_lossy()),
            Err(error) => unit.diagnostics.push(Diagnostic {
                code: "DROPIN_UNREADABLE".to_string(),
                severity: "warning".to_string(),
                message: format!("cannot read {}: {}", conf.display(), error),
                path: Some(conf.to_string_lossy().into_owned()),
                line: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drop_in_reset_and_specifiers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let drop = dir.join("aa.service.d");
        std::fs::create_dir_all(&drop).unwrap();
        std::fs::write(
            dir.join("aa.service"),
            "[Unit]\nWants=b.service\n[Service]\nUser=nobody\nExecStart=/bin/old\nExecStartPre=/bin/pre %u %h\n",
        )
        .unwrap();
        std::fs::write(
            drop.join("10-override.conf"),
            "[Service]\nExecStart=\nExecStart=/bin/new\n",
        )
        .unwrap();

        let units = load_units(&dir).unwrap();
        let unit = &units["aa.service"];
        // ExecStart= reset clears /bin/old before /bin/new is appended
        assert_eq!(unit.exec_start.as_deref(), Some("/bin/new"));
        assert_eq!(
            unit.sections["Service"]["ExecStart"],
            vec!["/bin/new".to_string()]
        );
        // %u/%h resolve to the system manager, independently of Service.User
        assert_eq!(
            unit.sections["Service"]["ExecStartPre"],
            vec!["/bin/pre root /root".to_string()]
        );
        assert_eq!(unit.drop_ins.len(), 1);
        assert_eq!(unit.wants, vec!["b.service".to_string()]);
    }

    #[test]
    fn test_template_drop_in_merges_before_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let tpl = dir.join("foo@.service.d");
        let inst = dir.join("foo@bar.service.d");
        std::fs::create_dir_all(&tpl).unwrap();
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(
            dir.join("foo@bar.service"),
            "[Service]\nExecStart=/run/%i\n",
        )
        .unwrap();
        std::fs::write(tpl.join("10-tpl.conf"), "[Unit]\nDescription=tpl\n").unwrap();
        std::fs::write(inst.join("20-inst.conf"), "[Unit]\nDescription=inst\n").unwrap();

        let unit = load_unit(&dir.join("foo@bar.service")).unwrap();
        assert_eq!(unit.drop_ins.len(), 2);
        // instance drop-in wins on Description (merged after template —
        // last value wins for single-valued keys)
        assert_eq!(unit.sections["Unit"]["Description"].last().unwrap(), "inst");
        // %i expands to the instance name
        assert_eq!(unit.exec_start.as_deref(), Some("/run/bar"));
    }

    #[test]
    fn test_load_units_rejects_missing_path() {
        let missing = std::path::Path::new("/nonexistent-lens-sys-dir-xyz");
        assert!(load_units(missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn review3_single_file_links_match_directory_classification() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("real.service"),
            "[Unit]\nAfter=real.service\n",
        )
        .unwrap();
        symlink("real.service", dir.path().join("alias.service")).unwrap();
        symlink("/dev/null", dir.path().join("masked.service")).unwrap();
        symlink("missing.service", dir.path().join("broken.service")).unwrap();
        let all = load_units(dir.path()).unwrap();
        for name in ["alias.service", "masked.service", "broken.service"] {
            let single = load_units(&dir.path().join(name)).unwrap();
            let unit = &single[name];
            assert_eq!(unit.masked, all[name].masked);
            assert_eq!(unit.alias_of, all[name].alias_of);
            assert_eq!(unit.after, all[name].after);
            assert_eq!(unit.diagnostics, all[name].diagnostics);
        }
    }

    #[test]
    fn test_load_units_merged_precedence_and_cross_dir_dropins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let etc = root.join("etc/systemd/system");
        let lib = root.join("usr/lib/systemd/system");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::create_dir_all(&lib).unwrap();

        // Vendor unit + vendor drop-in; /etc overrides the unit file and
        // adds a second drop-in.
        std::fs::write(
            lib.join("svc.service"),
            "[Service]\nExecStart=/bin/vendor\n",
        )
        .unwrap();
        std::fs::create_dir_all(lib.join("svc.service.d")).unwrap();
        std::fs::write(
            lib.join("svc.service.d/50-vendor.conf"),
            "[Service]\nEnvironment=V=vendor\n",
        )
        .unwrap();
        std::fs::write(etc.join("svc.service"), "[Service]\nExecStart=/bin/local\n").unwrap();
        std::fs::create_dir_all(etc.join("svc.service.d")).unwrap();
        std::fs::write(
            etc.join("svc.service.d/90-local.conf"),
            "[Service]\nEnvironment=E=local\n",
        )
        .unwrap();
        // Masked in /etc shadows the vendor unit completely.
        #[cfg(unix)]
        std::os::unix::fs::symlink("/dev/null", etc.join("masked.service")).unwrap();
        std::fs::write(lib.join("masked.service"), "[Service]\nExecStart=/bin/m\n").unwrap();
        // Vendor-only unit still loads.
        std::fs::write(
            lib.join("vendor-only.service"),
            "[Service]\nExecStart=/bin/v\n",
        )
        .unwrap();

        let dirs: Vec<std::path::PathBuf> = vec![etc.clone(), lib.clone()];
        let units = load_units_merged(&dirs).unwrap();

        // /etc unit file wins; drop-ins from both dirs merged.
        let svc = &units["svc.service"];
        assert_eq!(svc.exec_start.as_deref(), Some("/bin/local"));
        assert_eq!(svc.drop_ins.len(), 2);
        assert_eq!(
            svc.sections["Service"]["Environment"],
            vec!["V=vendor".to_string(), "E=local".to_string()]
        );
        // /etc mask shadows the vendor file.
        assert!(units["masked.service"].masked);
        // Vendor-only unit present.
        assert_eq!(
            units["vendor-only.service"].exec_start.as_deref(),
            Some("/bin/v")
        );
    }

    #[test]
    fn test_load_units_merged_dropin_only_stub() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let etc = root.join("etc");
        std::fs::create_dir_all(etc.join("ghost.service.d")).unwrap();
        std::fs::write(
            etc.join("ghost.service.d/10-x.conf"),
            "[Service]\nEnvironment=X=1\n",
        )
        .unwrap();
        let dirs: Vec<std::path::PathBuf> = vec![etc];
        let units = load_units_merged(&dirs).unwrap();
        let ghost = &units["ghost.service"];
        assert_eq!(ghost.sections["Service"]["Environment"], vec!["X=1"]);
    }

    #[test]
    fn test_syntax_and_missing_reference_diagnostics() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bad.service"),
            "[Unit\nRequires=nope.service\ngarbage line without equals\n[Service]\nExecStart=/bin/x\n",
        )
        .unwrap();

        let units = load_units(&dir).unwrap();
        let diags = &units["bad.service"].diagnostics;
        assert!(diags.iter().any(|d| d.code == "SYNTAX_SECTION_HEADER"));
        assert!(diags.iter().any(|d| d.code == "SYNTAX_GARBAGE_LINE"));
        // `Requires=nope.service` landed after the unclosed `[Unit`, so it
        // was ignored as garbage-adjacent syntax; use a clean section to
        // pin the missing-reference check.
        std::fs::write(
            dir.join("refs.service"),
            "[Unit]\nRequires=nope.service\n[Service]\nExecStart=/bin/y\n",
        )
        .unwrap();
        let units = load_units(&dir).unwrap();
        let diags = &units["refs.service"].diagnostics;
        assert!(diags
            .iter()
            .any(|d| d.code == "UNIT_REF_MISSING" && d.message.contains("nope.service")));
    }

    #[test]
    fn test_missing_reference_resolves_via_template() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("getty@.service"),
            "[Service]\nExecStart=/sbin/agetty\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("multi-user.target"),
            "[Unit]\nWants=getty@tty1.service\n",
        )
        .unwrap();

        let units = load_units(&dir).unwrap();
        assert!(units["multi-user.target"]
            .diagnostics
            .iter()
            .all(|d| d.code != "UNIT_REF_MISSING"));
    }
}
