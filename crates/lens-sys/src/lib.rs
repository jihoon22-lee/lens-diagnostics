pub mod dag;
pub mod diff;
pub mod loader;
pub mod model;
pub mod parser;

pub use dag::{CyclePath, OrderingConstraint, OrderingGraph};
pub use diff::diff_systemd;
pub use loader::{load_unit, load_units, load_units_merged, SYSTEMD_SEARCH_DIRS};
pub use model::*;
pub use parser::{apply_drop_in, expand_specifiers, is_template_name, parse_unit_content};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_parse_unit_and_specifiers() {
        let content = r#"
[Unit]
Description=My Service %i on %n
Wants=network.target network-online.target
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/my-daemon --instance=%i --user=%u
"#;
        let unit = parse_unit_content(
            content,
            "my-daemon@test.service",
            Some("/etc/systemd/system/my-daemon@test.service"),
        );
        assert_eq!(unit.name, "my-daemon@test.service");
        assert_eq!(unit.wants, vec!["network-online.target", "network.target"]);
        assert_eq!(unit.after, vec!["network.target"]);
        assert_eq!(
            unit.exec_start,
            Some("/usr/bin/my-daemon --instance=test --user=root".to_string())
        );
    }

    #[test]
    fn test_drop_in_override() {
        let base_content = r#"
[Unit]
Description=Base Service
After=syslog.target

[Service]
ExecStart=/usr/bin/old-daemon
"#;
        let mut unit = parse_unit_content(base_content, "base.service", None);

        let drop_in = r#"
[Unit]
Wants=redis.service

[Service]
ExecStart=
ExecStart=/usr/bin/new-daemon --fast
"#;
        apply_drop_in(
            &mut unit,
            drop_in,
            "/etc/systemd/system/base.service.d/override.conf",
        );

        assert_eq!(unit.wants, vec!["redis.service"]);
        assert_eq!(
            unit.exec_start,
            Some("/usr/bin/new-daemon --fast".to_string())
        );
        assert_eq!(unit.drop_ins.len(), 1);
    }

    #[test]
    fn test_cycle_detection() {
        let mut units = BTreeMap::new();

        let u1 = parse_unit_content("[Unit]\nBefore=b.service\n", "a.service", None);
        let u2 = parse_unit_content("[Unit]\nBefore=c.service\n", "b.service", None);
        let u3 = parse_unit_content("[Unit]\nBefore=a.service\n", "c.service", None);

        units.insert("a.service".to_string(), u1);
        units.insert("b.service".to_string(), u2);
        units.insert("c.service".to_string(), u3);

        let graph = OrderingGraph::build(&units);
        let cycles = graph.find_cycles();

        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec!["a.service", "b.service", "c.service"]);
        assert!(graph.topological_sort().is_none());
    }

    #[test]
    fn test_cycle_path_has_directive_and_origin() {
        let mut units = BTreeMap::new();
        units.insert(
            "a.service".to_string(),
            parse_unit_content(
                "[Unit]\nBefore=b.service\nAfter=c.service\n",
                "a.service",
                Some("/etc/systemd/system/a.service"),
            ),
        );
        units.insert(
            "b.service".to_string(),
            parse_unit_content(
                "[Unit]\nBefore=c.service\n",
                "b.service",
                Some("/etc/systemd/system/b.service"),
            ),
        );
        units.insert(
            "c.service".to_string(),
            parse_unit_content(
                "[Unit]\n",
                "c.service",
                Some("/etc/systemd/system/c.service"),
            ),
        );

        let graph = OrderingGraph::build(&units);
        let paths = graph.find_cycle_paths();
        assert_eq!(paths.len(), 1);
        let edges = &paths[0].edges;
        assert_eq!(edges.len(), 3);
        // Real directed hops, not an alphabetical member list.
        let hops: Vec<(&str, &str)> = edges
            .iter()
            .map(|e| (e.before.as_str(), e.after.as_str()))
            .collect();
        // The chain is contiguous and returns to its start.
        assert_eq!(hops.first().unwrap().0, hops.last().unwrap().1);
        for (w, (_, after)) in hops.iter().enumerate() {
            assert_eq!(hops[(w + 1) % hops.len()].0, *after);
        }
        // Every hop carries its directive and file:line origin.
        for e in edges {
            assert!(e.directive == "Before" || e.directive == "After");
            assert!(e.path.is_some());
            assert!(e.line.is_some());
        }
        // The After edge is declared by the unit that must start later.
        let after_edge = edges.iter().find(|e| e.directive == "After").unwrap();
        assert_eq!(after_edge.declared_by, "a.service");
        assert_eq!(after_edge.before, "c.service");
        assert_eq!(after_edge.after, "a.service");
        assert_eq!(after_edge.line, Some(3));
    }

    #[test]
    fn test_masked_alias_and_template_excluded_from_graph() {
        let mut units = BTreeMap::new();
        units.insert(
            "a.service".to_string(),
            parse_unit_content("[Unit]\nBefore=b.service\n", "a.service", None),
        );
        // Masked unit: present on disk but cannot be ordered.
        units.insert(
            "b.service".to_string(),
            SystemdUnit {
                name: "b.service".to_string(),
                masked: true,
                ..Default::default()
            },
        );
        // Template unit: not a concrete instance.
        units.insert(
            "tpl@.service".to_string(),
            parse_unit_content("[Unit]\nAfter=a.service\n", "tpl@.service", None),
        );
        // Alias stub: resolves to c.service for ordering.
        units.insert(
            "dbus-org.c.service".to_string(),
            SystemdUnit {
                name: "dbus-org.c.service".to_string(),
                alias_of: Some("c.service".to_string()),
                ..Default::default()
            },
        );
        units.insert(
            "c.service".to_string(),
            parse_unit_content("[Unit]\nAfter=a.service\n", "c.service", None),
        );
        // Edge via alias name resolves to the canonical unit.
        units.insert(
            "d.service".to_string(),
            parse_unit_content("[Unit]\nAfter=dbus-org.c.service\n", "d.service", None),
        );

        let graph = OrderingGraph::build(&units);
        assert!(!graph.all_nodes.contains("b.service"));
        assert!(!graph.all_nodes.contains("tpl@.service"));
        assert!(!graph.all_nodes.contains("dbus-org.c.service"));
        assert!(graph.adj["a.service"].contains("c.service"));
        assert!(graph.find_cycles().is_empty());
    }

    #[test]
    fn test_diff_systemd_reports_user_and_section_changes() {
        let make_snap = |unit: SystemdUnit| SystemdSnapshot {
            schema: SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            semantics: "systemd-255-subset-v1".to_string(),
            units: {
                let mut map = BTreeMap::new();
                map.insert(unit.name.clone(), unit);
                map
            },
            cycles: vec![],
            diagnostics: vec![],
        };

        let s1 = make_snap(parse_unit_content(
            "[Service]\nExecStart=/bin/a\n",
            "a.service",
            None,
        ));
        // Adding User= is invisible to the derived fields — only the
        // section matrix shows it.
        let s2 = make_snap(parse_unit_content(
            "[Service]\nUser=svc\nExecStart=/bin/a\n",
            "a.service",
            None,
        ));

        let diff = diff_systemd(&s1, &s2);
        assert_eq!(diff.modified_units.len(), 1);
        assert!(diff.modified_units[0]
            .details
            .iter()
            .any(|d| d.contains("[Service] User")));
    }

    #[test]
    fn test_diff_systemd() {
        let snap1 = SystemdSnapshot {
            schema: SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            semantics: "systemd-255-subset-v1".to_string(),
            units: {
                let mut map = BTreeMap::new();
                map.insert(
                    "a.service".to_string(),
                    parse_unit_content("[Service]\nExecStart=/bin/a", "a.service", None),
                );
                map
            },
            cycles: vec![],
            diagnostics: vec![],
        };

        let snap2 = SystemdSnapshot {
            schema: SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            semantics: "systemd-255-subset-v1".to_string(),
            units: {
                let mut map = BTreeMap::new();
                map.insert(
                    "a.service".to_string(),
                    parse_unit_content("[Service]\nExecStart=/bin/a_new", "a.service", None),
                );
                map.insert(
                    "b.service".to_string(),
                    parse_unit_content("[Service]\nExecStart=/bin/b", "b.service", None),
                );
                map
            },
            cycles: vec![],
            diagnostics: vec![],
        };

        let diff = diff_systemd(&snap1, &snap2);
        assert_eq!(diff.added_units, vec!["b.service"]);
        assert!(diff.removed_units.is_empty());
        assert_eq!(diff.modified_units.len(), 1);
        assert_eq!(diff.modified_units[0].unit, "a.service");
    }

    #[test]
    fn test_s01_diff_systemd_masked_and_alias_of_changes() {
        let make_snap = |unit: SystemdUnit| SystemdSnapshot {
            schema: SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            semantics: "systemd-255-subset-v1".to_string(),
            units: {
                let mut map = BTreeMap::new();
                map.insert(unit.name.clone(), unit);
                map
            },
            cycles: vec![],
            diagnostics: vec![],
        };

        let mut u1 = SystemdUnit {
            name: "a.service".to_string(),
            masked: false,
            alias_of: Some("old.service".to_string()),
            ..Default::default()
        };
        let mut u2 = u1.clone();
        u2.masked = true;
        u2.alias_of = Some("new.service".to_string());

        let diff = diff_systemd(&make_snap(u1.clone()), &make_snap(u2));
        assert_eq!(diff.modified_units.len(), 1);
        let details = &diff.modified_units[0].details;
        assert!(details.iter().any(|d| d.contains("Masked")));
        assert!(details.iter().any(|d| d.contains("AliasOf")));

        u1.masked = false;
    }

    #[test]
    fn test_s02_specifiers_dropin_user_reexpansion_and_template_graph() {
        let base_content = "[Unit]\nDescription=%n vs %N\n[Service]\nExecStart=/bin/run --user=%u --home=%h --base=%N --full=%n\n";
        let mut unit = parse_unit_content(base_content, "foo@bar.service", None);
        assert_eq!(
            unit.exec_start.as_deref(),
            Some("/bin/run --user=root --home=/root --base=foo@bar --full=foo@bar.service")
        );

        // Drop-in overriding User= must re-expand %u and %h in base unit directives.
        apply_drop_in(&mut unit, "[Service]\nUser=daemon_user\n", "/etc/foo.conf");
        assert_eq!(
            unit.exec_start.as_deref(),
            Some("/bin/run --user=daemon_user --home=/home/daemon_user --base=foo@bar --full=foo@bar.service")
        );

        // Template instance referenced in ordering graph resolves via foo@.service template.
        let mut units = BTreeMap::new();
        units.insert(
            "app.service".to_string(),
            parse_unit_content("[Unit]\nBefore=worker@1.service\n", "app.service", None),
        );
        units.insert(
            "worker@.service".to_string(),
            parse_unit_content("[Unit]\nBefore=app.service\n", "worker@.service", None),
        );

        let graph = OrderingGraph::build(&units);
        let cycles = graph.find_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec!["app.service", "worker@1.service"]);
    }
}
