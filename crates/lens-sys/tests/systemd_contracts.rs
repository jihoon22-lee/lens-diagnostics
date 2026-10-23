use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lens_sys::{
    diff_systemd, load_units, load_units_merged, parse_unit_content, OrderingGraph, SystemdSnapshot,
};

fn write(root: &Path, name: &str, content: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn template_instances_apply_their_own_dropins_in_both_loaders() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "start.target", "[Unit]\nRequires=a@x.service\n");
    write(
        root,
        "a@.service",
        "[Unit]\nRequires=b@%i.service\nAfter=b@%i.service\n",
    );
    write(root, "b@.service", "[Service]\nExecStart=/bin/true\n");
    write(
        root,
        "b@x.service.d/override.conf",
        "[Unit]\nAfter=a@x.service\n",
    );
    for units in [
        load_units(root).unwrap(),
        load_units_merged(&[root]).unwrap(),
    ] {
        let graph = OrderingGraph::build(&units).unwrap();
        assert_eq!(
            graph.find_cycles(),
            vec![vec!["a@x.service", "b@x.service"]]
        );
        let edges = &graph.find_cycle_paths()[0].edges;
        assert!(edges.iter().any(|edge| edge
            .path
            .as_deref()
            .is_some_and(|p| p.ends_with("b@x.service.d/override.conf"))));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("b@.service", root.join("b@x.service")).unwrap();
        let units = load_units(root).unwrap();
        assert_eq!(
            OrderingGraph::build(&units).unwrap().find_cycles(),
            vec![vec!["a@x.service", "b@x.service"]]
        );
    }
}

#[test]
fn empty_dependencies_cannot_hide_cycles_but_exec_start_can_reset() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.service", "[Unit]\nRequires=b.service\nAfter=b.service\n[Service]\nUser=nobody\nExecStart=/old %u %h\n");
    write(dir.path(), "b.service", "[Unit]\nAfter=a.service\n");
    write(
        dir.path(),
        "a.service.d/override.conf",
        "[Unit]\nAfter=\nRequires=\n[Service]\nUser=daemon\nExecStart=\nExecStart=/new %u %h\n",
    );
    let units = load_units(dir.path()).unwrap();
    assert_eq!(units["a.service"].requires, ["b.service"]);
    assert_eq!(
        units["a.service"].exec_start.as_deref(),
        Some("/new root /root")
    );
    assert_eq!(units["a.service"].sections["Service"]["ExecStart"].len(), 1);
    assert_eq!(
        OrderingGraph::build(&units).unwrap().find_cycles(),
        vec![vec!["a.service", "b.service"]]
    );
}

#[test]
fn merged_instance_dropins_use_filename_order_and_shadow_same_names() {
    let dir = tempfile::tempdir().unwrap();
    let high = dir.path().join("etc");
    let low = dir.path().join("vendor");
    write(&low, "a@.service", "[Service]\nExecStart=/base\n");
    write(
        &low,
        "a@.service.d/10-common.conf",
        "[Unit]\nWants=shadowed.service\n",
    );
    write(
        &high,
        "a@x.service.d/10-common.conf",
        "[Unit]\nWants=winner.service\n[Service]\nExecStart=\nExecStart=/early\n",
    );
    write(
        &low,
        "a@.service.d/90-late.conf",
        "[Service]\nExecStart=\nExecStart=/late\n",
    );
    let units = load_units_merged(&[&high, &low]).unwrap();
    let instance = &units["a@x.service"];
    assert_eq!(instance.wants, ["winner.service"]);
    assert_eq!(instance.exec_start.as_deref(), Some("/late"));
}

#[cfg(unix)]
#[test]
fn dependency_directory_changes_are_visible_to_diff() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.service", "[Unit]\nDescription=A\n");
    write(dir.path(), "b.service", "[Unit]\nDescription=B\n");
    let snapshot = || SystemdSnapshot {
        schema: lens_sys::SNAPSHOT_SCHEMA_V1.into(),
        version: "test".into(),
        semantics: "systemd-255-subset-v2".into(),
        units: load_units(dir.path()).unwrap(),
        cycles: vec![],
        diagnostics: vec![],
    };
    let before = snapshot();
    for suffix in ["wants", "requires"] {
        let deps = dir.path().join(format!("a.service.{suffix}"));
        fs::create_dir(&deps).unwrap();
        std::os::unix::fs::symlink("../b.service", deps.join("b.service")).unwrap();
    }
    let after = snapshot();
    for diff in [diff_systemd(&before, &after), diff_systemd(&after, &before)] {
        assert_eq!(diff.modified_units.len(), 1);
        assert!(diff.modified_units[0]
            .details
            .iter()
            .any(|s| s.contains("Wants")));
        assert!(diff.modified_units[0]
            .details
            .iter()
            .any(|s| s.contains("Requires")));
    }
}

#[test]
fn independent_parses_and_serialized_template_values_do_not_share_hidden_state() {
    let first = parse_unit_content("[Unit]\nBefore=app.service\n", "worker@.service", None);
    let _second = parse_unit_content("[Unit]\nBefore=other.service\n", "worker@.service", None);
    let units: BTreeMap<String, _> = BTreeMap::from([
        ("worker@.service".into(), first),
        (
            "app.service".into(),
            parse_unit_content("[Unit]\nBefore=worker@x.service\n", "app.service", None),
        ),
    ]);
    let units = serde_json::from_str(&serde_json::to_string(&units).unwrap()).unwrap();
    assert_eq!(
        OrderingGraph::build(&units).unwrap().find_cycles(),
        vec![vec!["app.service", "worker@x.service"]]
    );
}

#[test]
fn branching_templates_return_a_limit_error_in_both_loaders() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "app.service", "[Unit]\nWants=a@x.service\n");
    write(
        dir.path(),
        "a@.service",
        "[Unit]\nAfter=a@%i-x.service a@%i-y.service\n",
    );
    for result in [load_units(dir.path()), load_units_merged(&[dir.path()])] {
        assert!(matches!(
            result,
            Err(lens_core::LensError::LimitExceeded { .. })
        ));
    }
}

#[test]
fn graph_node_limit_accepts_boundary_and_rejects_one_more() {
    let mut units = BTreeMap::new();
    for index in 0..10_000 {
        let name = format!("unit-{index}.service");
        units.insert(name.clone(), parse_unit_content("[Unit]\n", &name, None));
    }
    assert_eq!(
        OrderingGraph::build(&units).unwrap().all_nodes.len(),
        10_000
    );
    units.insert(
        "extra.service".into(),
        parse_unit_content("[Unit]\n", "extra.service", None),
    );
    assert!(matches!(
        OrderingGraph::build(&units),
        Err(lens_core::LensError::LimitExceeded { .. })
    ));
}
