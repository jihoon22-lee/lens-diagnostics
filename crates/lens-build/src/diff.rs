use crate::model::{BuildDiff, IncludeOrderChange, ParsedUnit, UnitDiff, DIFF_SCHEMA_V1};
use std::collections::{BTreeMap, BTreeSet};

pub fn diff_compilations(baseline: &[ParsedUnit], candidate: &[ParsedUnit]) -> BuildDiff {
    let base_map: BTreeMap<&str, &ParsedUnit> =
        baseline.iter().map(|u| (u.file.as_str(), u)).collect();
    let cand_map: BTreeMap<&str, &ParsedUnit> =
        candidate.iter().map(|u| (u.file.as_str(), u)).collect();

    let all_files: BTreeSet<&str> = base_map.keys().chain(cand_map.keys()).copied().collect();

    let mut added_units = Vec::new();
    let mut removed_units = Vec::new();
    let mut modified_units = Vec::new();

    for file in all_files {
        match (base_map.get(file), cand_map.get(file)) {
            (None, Some(_)) => added_units.push(file.to_string()),
            (Some(_), None) => removed_units.push(file.to_string()),
            (Some(b), Some(c)) => {
                let mut base_flags: BTreeSet<String> = b.flags.iter().cloned().collect();
                let mut cand_flags: BTreeSet<String> = c.flags.iter().cloned().collect();

                if b.standard != c.standard {
                    if let Some(ref std) = b.standard {
                        base_flags.insert(format!("-std={std}"));
                    }
                    if let Some(ref std) = c.standard {
                        cand_flags.insert(format!("-std={std}"));
                    }
                }

                if b.compiler != c.compiler {
                    if !b.compiler.is_empty() {
                        base_flags.insert(format!("compiler:{}", b.compiler));
                    }
                    if !c.compiler.is_empty() {
                        cand_flags.insert(format!("compiler:{}", c.compiler));
                    }
                }

                let forced_include_order =
                    (b.forced_includes != c.forced_includes).then(|| IncludeOrderChange {
                        before: b.forced_includes.clone(),
                        after: c.forced_includes.clone(),
                    });
                if forced_include_order.is_some() {
                    for fi in &b.forced_includes {
                        base_flags.insert(format!("-include {fi}"));
                    }
                    for fi in &c.forced_includes {
                        cand_flags.insert(format!("-include {fi}"));
                    }
                }

                let added_flags: Vec<String> =
                    cand_flags.difference(&base_flags).cloned().collect();
                let removed_flags: Vec<String> =
                    base_flags.difference(&cand_flags).cloned().collect();

                let base_defs: BTreeSet<&str> = b.defines.iter().map(String::as_str).collect();
                let cand_defs: BTreeSet<&str> = c.defines.iter().map(String::as_str).collect();

                let added_defines: Vec<String> = cand_defs
                    .difference(&base_defs)
                    .map(|s| s.to_string())
                    .collect();
                let removed_defines: Vec<String> = base_defs
                    .difference(&cand_defs)
                    .map(|s| s.to_string())
                    .collect();

                let base_incs: BTreeSet<&str> = b.includes.iter().map(String::as_str).collect();
                let cand_incs: BTreeSet<&str> = c.includes.iter().map(String::as_str).collect();

                let added_includes: Vec<String> = cand_incs
                    .difference(&base_incs)
                    .map(|s| s.to_string())
                    .collect();
                let removed_includes: Vec<String> = base_incs
                    .difference(&cand_incs)
                    .map(|s| s.to_string())
                    .collect();

                if !added_flags.is_empty()
                    || !removed_flags.is_empty()
                    || !added_defines.is_empty()
                    || !removed_defines.is_empty()
                    || !added_includes.is_empty()
                    || !removed_includes.is_empty()
                    || forced_include_order.is_some()
                {
                    modified_units.push(UnitDiff {
                        file: file.to_string(),
                        added_flags,
                        removed_flags,
                        added_defines,
                        removed_defines,
                        added_includes,
                        removed_includes,
                        forced_include_order,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }

    BuildDiff {
        schema: DIFF_SCHEMA_V1.to_string(),
        added_units,
        removed_units,
        modified_units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_include_reordering_survives_parsing_and_json_roundtrip() {
        let parsed = |headers: &[&str]| {
            let mut arguments = vec!["cc".to_string(), "-c".into(), "main.c".into()];
            for header in headers {
                arguments.extend(["-include".to_string(), header.to_string()]);
            }
            crate::parse_command_entry(&crate::CompileCommandEntry {
                directory: "/project".into(),
                file: "main.c".into(),
                command: None,
                arguments: Some(arguments),
                output: None,
            })
        };
        let before = parsed(&["a.h", "b.h"]);
        let after = parsed(&["b.h", "a.h"]);
        let diff = diff_compilations(std::slice::from_ref(&before), &[after]);
        assert_eq!(diff.modified_units.len(), 1);
        let change = &diff.modified_units[0];
        assert!(change.added_flags.is_empty());
        assert!(change.removed_flags.is_empty());
        let order = change.forced_include_order.as_ref().unwrap();
        assert_eq!(order.before, ["a.h", "b.h"]);
        assert_eq!(order.after, ["b.h", "a.h"]);
        assert!(
            diff_compilations(std::slice::from_ref(&before), std::slice::from_ref(&before))
                .modified_units
                .is_empty()
        );
        let mut json = serde_json::to_value(change).unwrap();
        let restored: UnitDiff = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(&restored, change);
        json.as_object_mut().unwrap().remove("forced_include_order");
        let legacy: UnitDiff = serde_json::from_value(json).unwrap();
        assert!(legacy.forced_include_order.is_none());
    }

    #[test]
    fn test_diff_detects_standard_compiler_and_forced_includes() {
        let base = ParsedUnit {
            file: "/p/a.cpp".to_string(),
            directory: "/p".to_string(),
            compiler: "g++".to_string(),
            includes: vec![],
            defines: vec![],
            output: Some("a.o".to_string()),
            standard: Some("c++17".to_string()),
            flags: vec!["-Wall".to_string()],
            forced_includes: vec!["old_pch.h".to_string()],
        };
        let cand = ParsedUnit {
            file: "/p/a.cpp".to_string(),
            directory: "/p".to_string(),
            compiler: "clang++".to_string(),
            includes: vec![],
            defines: vec![],
            output: Some("a.o".to_string()),
            standard: Some("c++20".to_string()),
            flags: vec!["-Wall".to_string()],
            forced_includes: vec!["new_pch.h".to_string()],
        };

        let diff = diff_compilations(&[base], &[cand]);
        assert_eq!(diff.modified_units.len(), 1);
        let u = &diff.modified_units[0];
        assert!(u.added_flags.contains(&"-std=c++20".to_string()));
        assert!(u.removed_flags.contains(&"-std=c++17".to_string()));
        assert!(u.added_flags.contains(&"compiler:clang++".to_string()));
        assert!(u.removed_flags.contains(&"compiler:g++".to_string()));
        assert!(u.added_flags.contains(&"-include new_pch.h".to_string()));
        assert!(u.removed_flags.contains(&"-include old_pch.h".to_string()));
    }
}
