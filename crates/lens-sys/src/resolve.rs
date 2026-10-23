//! Bounded template expansion shared by file loading and in-memory graphs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

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

fn metadata_bytes(unit: &SystemdUnit) -> usize {
    let sections = unit.sections.iter().flat_map(|(section, keys)| {
        std::iter::once(section.len()).chain(keys.iter().flat_map(|(key, values)| {
            std::iter::once(key.len()).chain(values.iter().map(String::len))
        }))
    });
    sections
        .chain(
            unit.before
                .iter()
                .chain(&unit.after)
                .chain(&unit.wants)
                .chain(&unit.requires)
                .map(String::len),
        )
        .chain(unit.ordering_edges.iter().map(|e| {
            e.source
                .len()
                .saturating_add(e.target.len())
                .saturating_add(e.path.as_ref().map_or(0, String::len))
        }))
        .chain(unit.diagnostics.iter().map(|d| d.message.len()))
        .fold(unit.name.len(), usize::saturating_add)
}

/// Expand every referenced instance once, preserving deterministic queue order.
/// The callback applies the file loader's instance-specific configuration.
pub(crate) fn expand_instances(
    units: &mut BTreeMap<String, SystemdUnit>,
    mut instantiate: impl FnMut(&SystemdUnit, &str) -> Result<SystemdUnit>,
) -> Result<()> {
    if units.len() > MAX_UNITS {
        return Err(limit("unit count exceeds 10000"));
    }
    let mut bytes = units
        .values()
        .map(metadata_bytes)
        .fold(0usize, usize::saturating_add);
    if bytes > MAX_METADATA_BYTES {
        return Err(limit("expanded metadata exceeds 64 MiB"));
    }
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
        for target in unit
            .before
            .iter()
            .chain(&unit.after)
            .chain(&unit.wants)
            .chain(&unit.requires)
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
            bytes = bytes.saturating_add(metadata_bytes(&instance));
            if bytes > MAX_METADATA_BYTES {
                return Err(limit("expanded metadata exceeds 64 MiB"));
            }
            queue.push_back((target.clone(), depth + 1));
            units.insert(target, instance);
        }
    }
    Ok(())
}
