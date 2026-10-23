use crate::model::*;
use std::collections::{BTreeSet, HashSet};

pub fn diff_systemd(baseline: &SystemdSnapshot, candidate: &SystemdSnapshot) -> SystemdDiff {
    let mut added_units = Vec::new();
    let mut removed_units = Vec::new();
    let mut modified_units = Vec::new();

    let all_unit_names: BTreeSet<String> = baseline
        .units
        .keys()
        .chain(candidate.units.keys())
        .cloned()
        .collect();

    for name in all_unit_names {
        match (baseline.units.get(&name), candidate.units.get(&name)) {
            (None, Some(_)) => {
                added_units.push(name);
            }
            (Some(_), None) => {
                removed_units.push(name);
            }
            (Some(b), Some(c)) => {
                // Effective dependencies also include .wants/.requires links,
                // which do not appear in the section/key matrix.
                let mut details = Vec::new();
                for (key, before, after) in [
                    ("Wants", &b.wants, &c.wants),
                    ("Requires", &b.requires, &c.requires),
                ] {
                    let before: BTreeSet<_> = before.iter().collect();
                    let after: BTreeSet<_> = after.iter().collect();
                    if before != after {
                        details.push(format!("[Effective] {key}: {before:?} -> {after:?}"));
                    }
                }

                if b.masked != c.masked {
                    details.push(format!("[Meta] Masked: {:?} -> {:?}", b.masked, c.masked));
                }
                if b.alias_of != c.alias_of {
                    details.push(format!(
                        "[Meta] AliasOf: {:?} -> {:?}",
                        b.alias_of, c.alias_of
                    ));
                }

                let section_names: BTreeSet<&String> =
                    b.sections.keys().chain(c.sections.keys()).collect();
                for sec in section_names {
                    let bm = b.sections.get(sec);
                    let cm = c.sections.get(sec);
                    let keys: BTreeSet<&String> = bm
                        .into_iter()
                        .flat_map(|m| m.keys())
                        .chain(cm.into_iter().flat_map(|m| m.keys()))
                        .collect();
                    for key in keys {
                        let bv = bm.and_then(|m| m.get(key));
                        let cv = cm.and_then(|m| m.get(key));
                        if bv != cv {
                            details.push(format!("[{}] {}: {:?} -> {:?}", sec, key, bv, cv));
                        }
                    }
                }

                if b.drop_ins != c.drop_ins {
                    details.push(format!(
                        "Drop-ins changed from {:?} to {:?}",
                        b.drop_ins, c.drop_ins
                    ));
                }

                if !details.is_empty() {
                    modified_units.push(UnitChange {
                        unit: name,
                        kind: "modified".to_string(),
                        details,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }

    let base_cycles_set: HashSet<Vec<String>> = baseline.cycles.iter().cloned().collect();
    let cand_cycles_set: HashSet<Vec<String>> = candidate.cycles.iter().cloned().collect();

    let mut new_cycles: Vec<Vec<String>> = cand_cycles_set
        .difference(&base_cycles_set)
        .cloned()
        .collect();
    new_cycles.sort();

    let mut resolved_cycles: Vec<Vec<String>> = base_cycles_set
        .difference(&cand_cycles_set)
        .cloned()
        .collect();
    resolved_cycles.sort();

    SystemdDiff {
        schema: DIFF_SCHEMA_V1.to_string(),
        added_units,
        removed_units,
        modified_units,
        new_cycles,
        resolved_cycles,
    }
}
