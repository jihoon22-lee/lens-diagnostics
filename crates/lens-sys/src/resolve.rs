//! Bounded template expansion shared by file loading and in-memory graphs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::mem::size_of;

use lens_core::{LensError, Result};

use crate::model::SystemdUnit;
use crate::parser::{is_template_name, template_name_of};

pub(crate) const MAX_UNITS: usize = 10_000;
pub(crate) const MAX_EDGES: usize = 100_000;
const MAX_EXPANSION_DEPTH: usize = 32;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn limit(message: &str) -> LensError {
    LensError::LimitExceeded {
        message: format!("systemd analysis: {message}"),
    }
}

fn vec_storage<T>(values: &Vec<T>) -> usize {
    values.capacity().saturating_mul(size_of::<T>())
}

fn string_list_bytes(values: &Vec<String>) -> usize {
    values
        .iter()
        .map(String::capacity)
        .fold(vec_storage(values), usize::saturating_add)
}

// Account for every owned field, including container storage and cloned
// provenance paths. The per-entry allowance covers BTree node bookkeeping;
// this is a conservative metadata budget, not an exact process RSS limit.
fn metadata_bytes(unit: &SystemdUnit) -> usize {
    let SystemdUnit {
        name,
        path,
        sections,
        wants,
        requires,
        before,
        after,
        exec_start,
        drop_ins,
        ordering_edges,
        masked: _,
        alias_of,
        aliases,
        diagnostics,
    } = unit;
    let mut bytes = size_of::<SystemdUnit>().saturating_add(name.capacity());
    for value in [path, exec_start, alias_of] {
        bytes = bytes.saturating_add(value.as_ref().map_or(0, String::capacity));
    }
    for values in [wants, requires, before, after, drop_ins, aliases] {
        bytes = bytes.saturating_add(string_list_bytes(values));
    }
    const NODE_OVERHEAD: usize = 3 * size_of::<usize>();
    for (section, keys) in sections {
        bytes = bytes
            .saturating_add(size_of::<(String, BTreeMap<String, Vec<String>>)>())
            .saturating_add(NODE_OVERHEAD)
            .saturating_add(section.capacity());
        for (key, values) in keys {
            bytes = bytes
                .saturating_add(size_of::<(String, Vec<String>)>())
                .saturating_add(NODE_OVERHEAD)
                .saturating_add(key.capacity())
                .saturating_add(string_list_bytes(values));
        }
    }
    bytes = bytes.saturating_add(vec_storage(ordering_edges));
    for edge in ordering_edges {
        for value in [&edge.source, &edge.target, &edge.directive] {
            bytes = bytes.saturating_add(value.capacity());
        }
        bytes = bytes.saturating_add(edge.path.as_ref().map_or(0, String::capacity));
    }
    bytes = bytes.saturating_add(vec_storage(diagnostics));
    for diagnostic in diagnostics {
        for value in [&diagnostic.code, &diagnostic.severity, &diagnostic.message] {
            bytes = bytes.saturating_add(value.capacity());
        }
        bytes = bytes.saturating_add(diagnostic.path.as_ref().map_or(0, String::capacity));
    }
    bytes
}

fn entry_bytes(name: &String, unit: &SystemdUnit) -> usize {
    metadata_bytes(unit)
        .saturating_add(name.capacity())
        .saturating_add(size_of::<String>() + 3 * size_of::<usize>())
}

pub(crate) fn validate_units(units: &BTreeMap<String, SystemdUnit>) -> Result<usize> {
    if units.len() > MAX_UNITS {
        return Err(limit("unit count exceeds 10000"));
    }
    let bytes = units
        .iter()
        .map(|(name, unit)| entry_bytes(name, unit))
        .fold(0usize, usize::saturating_add);
    if bytes > MAX_METADATA_BYTES {
        return Err(limit("expanded metadata exceeds 64 MiB"));
    }
    Ok(bytes)
}

/// Expand every referenced instance once, preserving deterministic queue order.
/// The callback applies the file loader's instance-specific configuration.
pub(crate) fn expand_instances(
    units: &mut BTreeMap<String, SystemdUnit>,
    mut instantiate: impl FnMut(&SystemdUnit, &str) -> Result<SystemdUnit>,
) -> Result<()> {
    let mut bytes = validate_units(units)?;
    let aliases: BTreeMap<String, String> = units
        .iter()
        .filter_map(|(name, unit)| unit.alias_of.clone().map(|target| (name.clone(), target)))
        .collect();
    let mut queue: VecDeque<(String, usize)> = units
        .iter()
        .filter(|(name, unit)| !is_template_name(name) && !unit.masked && unit.alias_of.is_none())
        .map(|(name, _)| (name.clone(), 0))
        .collect();
    let mut references = 0usize;
    while let Some((name, depth)) = queue.pop_front() {
        let unit = &units[&name];
        let mut targets = BTreeSet::new();
        // Provenance edges and flattened ordering lists describe the same
        // references. Match graph construction: prefer edges when present.
        let flattened_ordering = unit
            .before
            .iter()
            .chain(&unit.after)
            .filter(|_| unit.ordering_edges.is_empty());
        for target in unit
            .wants
            .iter()
            .chain(&unit.requires)
            .chain(flattened_ordering)
            .chain(unit.ordering_edges.iter().map(|e| &e.target))
        {
            references += 1;
            if references > MAX_EDGES {
                return Err(limit("dependency references exceed 100000"));
            }
            targets.insert(aliases.get(target).unwrap_or(target).clone());
        }
        for target in targets {
            if units.contains_key(&target) {
                continue;
            }
            let Some(template) = template_name_of(&target).and_then(|name| units.get(&name)) else {
                continue;
            };
            if template.masked || template.alias_of.is_some() {
                continue;
            }
            if units.len() >= MAX_UNITS || depth >= MAX_EXPANSION_DEPTH || target.len() > 255 {
                return Err(limit(
                    "template expansion exceeds the unit, depth or name limit",
                ));
            }
            let instance = instantiate(template, &target)?;
            bytes = bytes.saturating_add(entry_bytes(&target, &instance));
            if bytes > MAX_METADATA_BYTES {
                return Err(limit("expanded metadata exceeds 64 MiB"));
            }
            queue.push_back((target.clone(), depth + 1));
            units.insert(target, instance);
        }
    }
    Ok(())
}
