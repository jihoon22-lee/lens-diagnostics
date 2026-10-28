//! Persistent CLI preferences. Never consumed by the MCP server.
use crate::cli::{Cli, CollectSource, Commands, DiskCommands, FailOn, OutputFormat};
use clap::{parser::ValueSource, ArgMatches, ValueEnum};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: Option<u32>,
    #[serde(default)]
    output: Output,
    #[serde(default)]
    policy: Policy,
    #[serde(default)]
    collect: BTreeMap<String, Collection>,
    #[serde(default)]
    snapshot: BTreeMap<String, Snapshot>,
    #[serde(default)]
    disk: Disk,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    format: Option<String>,
    quiet: Option<bool>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    fail_on: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Disk {
    max_depth: Option<usize>,
    exclude: Option<Vec<String>>,
    one_file_system: Option<bool>,
    parallel: Option<bool>,
    top: Option<usize>,
    min_size: Option<u64>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Collection {
    source: Option<String>,
    target: Option<String>,
    output: Option<PathBuf>,
    since: Option<String>,
    lines: Option<usize>,
    timeout: Option<u64>,
    #[serde(default)]
    user: Option<bool>,
    #[serde(default)]
    redact: Option<bool>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    output: Option<PathBuf>,
    procfs: Option<PathBuf>,
    systemd_dir: Option<PathBuf>,
    log: Option<PathBuf>,
    disk: Option<PathBuf>,
    env: Option<PathBuf>,
    test: Option<PathBuf>,
    trace: Option<PathBuf>,
    #[serde(default)]
    redact: Option<bool>,
}
pub struct Resolved {
    pub details: String,
    pub files: Vec<PathBuf>,
    pub boundary: PathBuf,
    profiles: BTreeMap<String, (Collection, PathBuf)>,
    snapshots: BTreeMap<String, (Snapshot, PathBuf)>,
}
fn read(path: &Path) -> Result<Config, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut config: Config =
        toml::from_str(&content).map_err(|e| format!("{}: {e}", path.display()))?;
    if config.schema_version != Some(1) {
        return Err(format!("{}: schema_version must be 1", path.display()));
    }
    if let Some(v) = &config.output.format {
        OutputFormat::from_str(v, false)
            .map_err(|e| format!("{}: output.format: {e}", path.display()))?;
    }
    if let Some(v) = &config.policy.fail_on {
        FailOn::from_str(v, false)
            .map_err(|e| format!("{}: policy.fail_on: {e}", path.display()))?;
    }
    let base = path.parent().unwrap_or(Path::new("."));
    for p in config.collect.values_mut() {
        if let Some(v) = &p.source {
            CollectSource::from_str(v, false).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if let Some(v) = &mut p.output {
            *v = base.join(&*v);
        }
    }
    for p in config.snapshot.values_mut() {
        for v in [
            &mut p.output,
            &mut p.procfs,
            &mut p.systemd_dir,
            &mut p.log,
            &mut p.disk,
            &mut p.env,
            &mut p.test,
            &mut p.trace,
        ]
        .into_iter()
        .flatten()
        {
            *v = base.join(&*v);
        }
    }
    Ok(config)
}
fn discover(cwd: &Path, home: Option<&Path>) -> (Option<PathBuf>, PathBuf) {
    let mut dir = cwd;
    loop {
        let candidate = dir.join("lens.toml");
        if candidate.is_file() {
            return (Some(candidate), dir.to_owned());
        }
        if dir.join(".git").exists() || home == Some(dir) {
            return (None, dir.to_owned());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return (None, dir.to_owned()),
        }
    }
}
impl Cli {
    pub fn resolve_config(&mut self, matches: &ArgMatches, tty: bool) -> Result<Resolved, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let home = std::env::var_os("HOME").map(PathBuf::from);

        let user = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .map(|p| p.join("lens/config.toml"));
        self.resolve_at(matches, tty, &cwd, home.as_deref(), user)
    }
    fn resolve_at(
        &mut self,
        matches: &ArgMatches,
        tty: bool,
        cwd: &Path,
        home: Option<&Path>,
        user: Option<PathBuf>,
    ) -> Result<Resolved, String> {
        let (project, boundary) = discover(cwd, home);
        let paths: Vec<PathBuf> = if self.no_config {
            vec![]
        } else if let Some(p) = &self.config {
            vec![p.clone()]
        } else {
            user.filter(|p| p.is_file())
                .into_iter()
                .chain(project)
                .collect()
        };
        let mut result = Resolved {
            details: String::new(),
            files: paths.clone(),
            boundary,
            profiles: BTreeMap::new(),
            snapshots: BTreeMap::new(),
        };
        let mut format = None;
        let mut quiet = None;
        let mut fail_on = None;
        let mut disk = Disk::default();
        for path in paths {
            let c = read(&path)?;
            // User personal presentation wins over project presentation.
            if format.is_none() {
                format = c.output.format;
            }
            if quiet.is_none() {
                quiet = c.output.quiet;
            }
            if c.policy.fail_on.is_some() {
                fail_on = c.policy.fail_on;
            }
            if c.disk.max_depth.is_some() {
                disk.max_depth = c.disk.max_depth;
            }
            if c.disk.exclude.is_some() {
                disk.exclude = c.disk.exclude;
            }
            if c.disk.one_file_system.is_some() {
                disk.one_file_system = c.disk.one_file_system;
            }
            if c.disk.parallel.is_some() {
                disk.parallel = c.disk.parallel;
            }
            if c.disk.top.is_some() {
                disk.top = c.disk.top;
            }
            if c.disk.min_size.is_some() {
                disk.min_size = c.disk.min_size;
            }
            macro_rules! merge { ($dst:expr, $src:expr, $($field:ident),+) => { $(if $src.$field.is_some() { $dst.$field = $src.$field; })+ }; }
            for (name, profile) in c.snapshot {
                let (dst, _) = result
                    .snapshots
                    .entry(name)
                    .or_insert_with(|| (Snapshot::default(), PathBuf::new()));
                merge!(
                    dst,
                    profile,
                    output,
                    procfs,
                    systemd_dir,
                    log,
                    disk,
                    env,
                    test,
                    trace,
                    redact
                );
            }
            for (name, profile) in c.collect {
                let (dst, _) = result
                    .profiles
                    .entry(name)
                    .or_insert_with(|| (Collection::default(), PathBuf::new()));
                merge!(dst, profile, source, target, output, since, lines, timeout, user, redact);
            }
        }
        for (name, (p, _)) in &result.profiles {
            if p.source.is_none()
                || p.target.as_deref().is_none_or(str::is_empty)
                || p.output.is_none()
            {
                return Err(format!("collect.{name}: source, target and output are required after merging configuration"));
            }
            if p.lines.is_some_and(|v| !(1..=1_000_000).contains(&v))
                || p.timeout.is_some_and(|v| !(1..=300).contains(&v))
            {
                return Err(format!(
                    "collect.{name}: lines must be 1..1000000 and timeout 1..300 seconds"
                ));
            }
        }
        result.details = format!(
            "disk: {disk:?}\ncollections: {:?}\nsnapshots: {:?}",
            result.profiles, result.snapshots
        );
        if self.format.is_none() && !self.json {
            self.format = format
                .as_deref()
                .map(|s| OutputFormat::from_str(s, false).expect("validated"));
            if self.format.is_none() && tty {
                self.format = Some(OutputFormat::Text);
            }
        }
        if matches.value_source("quiet") != Some(ValueSource::CommandLine) {
            self.quiet = quiet.unwrap_or(false);
        }
        if matches.value_source("fail_on") != Some(ValueSource::CommandLine) {
            self.fail_on = fail_on
                .as_deref()
                .map(|s| FailOn::from_str(s, false).expect("validated"))
                .unwrap_or(FailOn::Fail);
        }
        if let Commands::Inspect { path, as_kind } = &self.command {
            let has_disk_settings = disk.max_depth.is_some()
                || disk.exclude.is_some()
                || disk.one_file_system.is_some()
                || disk.parallel.is_some()
                || disk.top.is_some();
            if has_disk_settings
                && (matches!(as_kind, Some(crate::cli::AsKind::Disk))
                    || (as_kind.is_none()
                        && matches!(
                            lens_ops::detect::detect(path),
                            Ok(lens_ops::detect::InputKind::Directory)
                        )))
            {
                self.command = Commands::Disk {
                    action: DiskCommands::Scan {
                        path: path.clone(),
                        parallel: disk.parallel.unwrap_or(false),
                        max_depth: disk.max_depth,
                        exclude: disk.exclude.clone().unwrap_or_default(),
                        one_file_system: disk.one_file_system.unwrap_or(false),
                        top: disk.top.unwrap_or(0),
                    },
                };
            }
        }
        if let Commands::Disk { action } = &mut self.command {
            let m = matches
                .subcommand()
                .and_then(|(_, m)| m.subcommand())
                .map(|(_, m)| m);
            let explicit =
                |key| m.is_some_and(|m| m.value_source(key) == Some(ValueSource::CommandLine));
            match action {
                DiskCommands::Scan {
                    max_depth,
                    exclude,
                    one_file_system,
                    parallel,
                    top,
                    ..
                } => {
                    if !explicit("max_depth") {
                        *max_depth = disk.max_depth;
                    }
                    if !explicit("exclude") {
                        *exclude = disk.exclude.unwrap_or_default();
                    }
                    if !explicit("one_file_system") {
                        *one_file_system = disk.one_file_system.unwrap_or(false);
                    }
                    if !explicit("parallel") {
                        *parallel = disk.parallel.unwrap_or(false);
                    }
                    if !explicit("top") {
                        *top = disk.top.unwrap_or(*top);
                    }
                }
                DiskCommands::Duplicates {
                    max_depth,
                    exclude,
                    one_file_system,
                    min_size,
                    ..
                } => {
                    if !explicit("max_depth") {
                        *max_depth = disk.max_depth;
                    }
                    if !explicit("exclude") {
                        *exclude = disk.exclude.unwrap_or_default();
                    }
                    if !explicit("one_file_system") {
                        *one_file_system = disk.one_file_system.unwrap_or(false);
                    }
                    if !explicit("min_size") {
                        *min_size = disk.min_size.unwrap_or(1024);
                    }
                }
                _ => {}
            }
        }
        if let Commands::Run { name, options } = &self.command {
            if result.profiles.contains_key(name) && result.snapshots.contains_key(name) {
                return Err(format!("ambiguous run name {name:?}"));
            }
            if let Some((p, base)) = result.snapshots.get(name) {
                if options.target.is_some()
                    || options.since.is_some()
                    || options.lines.is_some()
                    || options.timeout.is_some()
                    || options.user.is_some()
                {
                    return Err("collection-only overrides cannot be used with a snapshot".into());
                }
                let path = |p: &Option<PathBuf>, override_path: &Option<PathBuf>| {
                    override_path
                        .clone()
                        .or_else(|| p.as_ref().map(|p| base.join(p)))
                };
                self.command = Commands::Snapshot {
                    output: path(&p.output, &options.output),
                    procfs: path(&p.procfs, &options.procfs),
                    systemd_dir: path(&p.systemd_dir, &options.systemd_dir),
                    log: path(&p.log, &options.log),
                    disk: path(&p.disk, &options.disk),
                    env: path(&p.env, &options.env),
                    test: path(&p.test, &options.test),
                    trace: path(&p.trace, &options.trace),
                    force: options.force,
                    redact: options.redact.or(p.redact).unwrap_or(false),
                };
                return Ok(result);
            }
            let (p, base) = result.profiles.get(name).ok_or_else(|| {
                format!("unknown collection {name:?}; declare [collect.{name}] in lens.toml")
            })?;
            if options.procfs.is_some()
                || options.systemd_dir.is_some()
                || options.log.is_some()
                || options.disk.is_some()
                || options.env.is_some()
                || options.test.is_some()
                || options.trace.is_some()
            {
                return Err("snapshot-only overrides cannot be used with a collection".into());
            }
            self.command = Commands::Collect {
                source: CollectSource::from_str(p.source.as_deref().expect("validated"), false)
                    .expect("validated"),
                target: options
                    .target
                    .clone()
                    .or_else(|| p.target.clone())
                    .expect("validated"),
                output: options
                    .output
                    .clone()
                    .unwrap_or_else(|| base.join(p.output.as_ref().expect("validated"))),
                user: options.user.or(p.user).unwrap_or(false),
                since: options.since.clone().or_else(|| p.since.clone()),
                lines: options.lines.or(p.lines).unwrap_or(1000),
                timeout: options.timeout.or(p.timeout).unwrap_or(30),
                force: options.force,
                redact: options.redact.or(p.redact).unwrap_or(false),
            };
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};
    fn resolve(args: &[&str], cwd: &Path, user: Option<PathBuf>, tty: bool) -> (Cli, Resolved) {
        let m = Cli::command().try_get_matches_from(args).unwrap();
        let mut c = Cli::from_arg_matches(&m).unwrap();
        let r = c.resolve_at(&m, tty, cwd, None, user).unwrap();
        (c, r)
    }
    #[test]
    fn precedence_and_minimal_equivalence() {
        let t = tempfile::tempdir().unwrap();
        let (c, _) = resolve(&["lens", "inspect"], t.path(), None, false);
        assert_eq!(c.format, None);
        std::fs::write(t.path().join("lens.toml"), "schema_version=1\n").unwrap();
        let (c, _) = resolve(&["lens", "inspect"], t.path(), None, false);
        assert_eq!(c.format, None);
        std::fs::write(
            t.path().join("lens.toml"),
            "schema_version=1\n[output]\nformat='json'\n[policy]\nfail_on='warn'\n",
        )
        .unwrap();
        let (c, _) = resolve(&["lens", "inspect"], t.path(), None, true);
        assert_eq!(c.format, Some(OutputFormat::Json));
        assert_eq!(c.fail_on, FailOn::Warn);
        let user = t.path().join("user.toml");
        std::fs::write(
            &user,
            "schema_version=1\n[output]\nformat='text'\n[policy]\nfail_on='fail'\n",
        )
        .unwrap();
        let (c, _) = resolve(&["lens", "inspect"], t.path(), Some(user.clone()), false);
        assert_eq!(c.format, Some(OutputFormat::Text));
        assert_eq!(c.fail_on, FailOn::Warn);
        let (c, _) = resolve(
            &[
                "lens",
                "inspect",
                "--format",
                "envelope",
                "--fail-on",
                "fail",
            ],
            t.path(),
            Some(user),
            false,
        );
        assert_eq!(c.format, Some(OutputFormat::Envelope));
        assert_eq!(c.fail_on, FailOn::Fail);
        let (c, r) = resolve(&["lens", "--no-config", "inspect"], t.path(), None, true);
        assert_eq!(c.format, Some(OutputFormat::Text));
        assert!(r.files.is_empty());
    }
    #[test]
    fn boundary_and_named_argv() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("lens.toml"), "schema_version=1\n[collect.api]\nsource='docker'\ntarget='a; touch nope'\noutput='out.lens'\n").unwrap();
        let sub = t.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let (c, _) = resolve(&["lens", "run", "api"], &sub, None, false);
        match c.command {
            Commands::Collect { target, output, .. } => {
                assert_eq!(target, "a; touch nope");
                assert_eq!(output, t.path().join("out.lens"));
            }
            _ => panic!(),
        }
        std::fs::write(sub.join(".git"), "gitdir: elsewhere").unwrap();
        assert!(discover(&sub, None).0.is_none());
    }
    #[test]
    fn disk_lists_replace_clear_and_false_preserve_intent() {
        let t = tempfile::tempdir().unwrap();
        let user = t.path().join("user.toml");
        std::fs::write(
            &user,
            "schema_version=1\n[disk]\nexclude=['cache']\none_file_system=true\nmax_depth=3\n",
        )
        .unwrap();
        std::fs::write(
            t.path().join("lens.toml"),
            "schema_version=1\n[disk]\nexclude=[]\none_file_system=false\n",
        )
        .unwrap();
        let (c, _) = resolve(
            &["lens", "disk", "scan"],
            t.path(),
            Some(user.clone()),
            false,
        );
        match c.command {
            Commands::Disk {
                action:
                    DiskCommands::Scan {
                        exclude,
                        one_file_system,
                        max_depth,
                        ..
                    },
            } => {
                assert!(exclude.is_empty());
                assert!(!one_file_system);
                assert_eq!(max_depth, Some(3));
            }
            _ => panic!(),
        }
        let (c, _) = resolve(
            &[
                "lens",
                "disk",
                "scan",
                "--exclude",
                "foo",
                "--max-depth",
                "1",
            ],
            t.path(),
            Some(user),
            false,
        );
        match c.command {
            Commands::Disk {
                action:
                    DiskCommands::Scan {
                        exclude, max_depth, ..
                    },
            } => {
                assert_eq!(exclude, vec!["foo"]);
                assert_eq!(max_depth, Some(1));
            }
            _ => panic!(),
        }
    }
    #[test]
    fn explicit_file_ignores_user_and_project_and_snapshot_paths_are_relative() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("selected.toml");
        std::fs::write(
            &p,
            "schema_version=1\n[snapshot.host]\nlog='logs/app.log'\noutput='host.lens'\n",
        )
        .unwrap();
        std::fs::write(t.path().join("lens.toml"), "invalid TOML").unwrap();
        let (c, r) = resolve(
            &["lens", "--config", p.to_str().unwrap(), "run", "host"],
            t.path(),
            Some(t.path().join("lens.toml")),
            false,
        );
        assert_eq!(r.files, vec![p]);
        match c.command {
            Commands::Snapshot { log, output, .. } => {
                assert_eq!(log, Some(t.path().join("logs/app.log")));
                assert_eq!(output, Some(t.path().join("host.lens")));
            }
            _ => panic!(),
        }
    }
    #[test]
    fn report_format_matrix() {
        let t = tempfile::tempdir().unwrap();
        let cases = [
            "inspect p",
            "diff a b",
            "doctor",
            "disk scan",
            "disk duplicates",
            "abi inspect p",
            "abi diff a b",
            "log inspect p",
            "log filter p",
            "log stats p",
            "log diff a b",
            "test parse p",
            "test diff a b",
            "trace analyze p",
            "trace diff a b",
            "sys inspect",
            "sys cycles",
            "sys diff a b",
            "build inspect p",
            "build impact p --header h",
            "build diff a b",
            "env inspect p",
            "env check p",
            "env diff a b",
            "net inspect",
            "net diff a b",
            "bundle inspect p",
            "bundle verify p",
            "bundle diff a b",
        ];
        for case in cases {
            for tty in [false, true] {
                let mut args = vec!["lens", "--no-config"];
                args.extend(case.split_whitespace());
                let (c, _) = resolve(&args, t.path(), None, tty);
                assert_eq!(c.format, tty.then_some(OutputFormat::Text), "{case}");
                for (name, format) in [
                    ("text", OutputFormat::Text),
                    ("json", OutputFormat::Json),
                    ("jsonl", OutputFormat::Jsonl),
                    ("envelope", OutputFormat::Envelope),
                ] {
                    let mut explicit = args.clone();
                    explicit.extend(["--format", name]);
                    let (c, _) = resolve(&explicit, t.path(), None, tty);
                    assert_eq!(c.format, Some(format), "{case}");
                }
            }
        }
    }
    #[test]
    fn named_profiles_merge_fields_with_origins_and_cli_overrides() {
        let t = tempfile::tempdir().unwrap();
        let prefs = t.path().join("prefs");
        std::fs::create_dir(&prefs).unwrap();
        let user = prefs.join("config.toml");
        std::fs::write(&user, "schema_version=1\n[collect.api]\nsource='docker'\ntarget='api'\noutput='user.lens'\nlines=42\nredact=true\n[snapshot.daily]\nlog='user.log'\nredact=true\n").unwrap();
        std::fs::write(t.path().join("lens.toml"), "schema_version=1\n[collect.api]\ntimeout=12\nredact=false\n[snapshot.daily]\noutput='daily.lens'\n").unwrap();
        let (c, _) = resolve(
            &["lens", "run", "api", "--lines", "7", "--output", "cli.lens"],
            t.path(),
            Some(user.clone()),
            false,
        );
        match c.command {
            Commands::Collect {
                target,
                output,
                lines,
                timeout,
                redact,
                ..
            } => {
                assert_eq!(target, "api");
                assert_eq!(output, PathBuf::from("cli.lens"));
                assert_eq!(lines, 7);
                assert_eq!(timeout, 12);
                assert!(!redact);
            }
            _ => panic!(),
        }
        let (c, _) = resolve(
            &["lens", "run", "daily", "--redact=false"],
            t.path(),
            Some(user),
            false,
        );
        match c.command {
            Commands::Snapshot {
                output,
                log,
                redact,
                ..
            } => {
                assert_eq!(output, Some(t.path().join("daily.lens")));
                assert_eq!(log, Some(prefs.join("user.log")));
                assert!(!redact);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn invalid_keys_and_version_rejected() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("lens.toml");
        for text in [
            "schema_version=2",
            "schema_version=1\nallow_root='/'",
            "schema_version=1\n[output]\nformat='typo'",
        ] {
            std::fs::write(&p, text).unwrap();
            assert!(read(&p).is_err());
        }
    }
}
