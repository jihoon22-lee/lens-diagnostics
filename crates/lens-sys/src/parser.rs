use crate::model::{Diagnostic, OrderingEdge, SystemdUnit};
use std::collections::BTreeMap;

/// Identity of the service manager; never derived from Service.User.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManagerContext {
    pub user: String,
    pub home: String,
    pub runtime_dir: String,
}
impl Default for ManagerContext {
    fn default() -> Self {
        Self {
            user: "root".into(),
            home: "/root".into(),
            runtime_dir: "/run".into(),
        }
    }
}

/// System-manager specifiers. `%u`/`%h` belong to the manager, not Service.User.
#[derive(Debug, Clone)]
struct SpecContext {
    unit_name: String,
    unit_name_without_suffix: String,
    prefix: String,
    instance: String,
    user: String,
    home: String,
    runtime_dir: String,
}

impl SpecContext {
    fn minimal(unit_name: &str, manager: &ManagerContext) -> Self {
        let (prefix, instance) = split_unit_name(unit_name);
        let unit_name_without_suffix = strip_unit_suffix(unit_name).to_string();
        SpecContext {
            unit_name: unit_name.to_string(),
            unit_name_without_suffix,
            prefix,
            instance,
            user: manager.user.clone(),
            home: manager.home.clone(),
            runtime_dir: manager.runtime_dir.clone(),
        }
    }
}

fn strip_unit_suffix(unit_name: &str) -> &str {
    unit_name
        .rfind('.')
        .map(|dot| &unit_name[..dot])
        .unwrap_or(unit_name)
}

fn split_unit_name(unit_name: &str) -> (String, String) {
    if let Some(at_pos) = unit_name.find('@') {
        let p = &unit_name[..at_pos];
        let rest = &unit_name[at_pos + 1..];
        let inst = rest.rfind('.').map(|dot| &rest[..dot]).unwrap_or(rest);
        (p.to_string(), inst.to_string())
    } else {
        let p = strip_unit_suffix(unit_name);
        (p.to_string(), String::new())
    }
}

pub fn expand_specifiers(value: &str, unit_name: &str) -> String {
    expand_specifiers_with_manager(value, unit_name, &ManagerContext::default())
}

pub fn expand_specifiers_with_manager(
    value: &str,
    unit_name: &str,
    manager: &ManagerContext,
) -> String {
    expand_with(&SpecContext::minimal(unit_name, manager), value)
}

/// `foo@.service` — a template unit declares no concrete instance and
/// must not participate in the ordering graph directly.
pub fn is_template_name(name: &str) -> bool {
    match (name.find('@'), name.rfind('.')) {
        (Some(at), Some(dot)) => at + 1 == dot,
        _ => false,
    }
}

/// For a concrete template instance `foo@bar.service`, return its template
/// unit name `foo@.service`.
pub fn template_name_of(name: &str) -> Option<String> {
    let at = name.find('@')?;
    let dot = name.rfind('.')?;
    if at + 1 < dot {
        Some(format!("{}@{}", &name[..at], &name[dot..]))
    } else {
        None
    }
}

fn expand_with(ctx: &SpecContext, value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next) = chars.peek() {
                chars.next();
                match next {
                    '%' => out.push('%'),
                    'n' => out.push_str(&ctx.unit_name),
                    'N' => out.push_str(&ctx.unit_name_without_suffix),
                    'p' => out.push_str(&ctx.prefix),
                    'i' | 'I' => out.push_str(&ctx.instance),
                    'u' => out.push_str(&ctx.user),
                    'h' => out.push_str(&ctx.home),
                    't' => out.push_str(&ctx.runtime_dir),
                    other => {
                        out.push('%');
                        out.push(other);
                    }
                }
            } else {
                out.push('%');
            }
        } else {
            out.push(ch);
        }
    }

    out
}

/// One raw assignment in file order: (line, section, key, raw value).
/// Kept ordered because `Key=` resets must apply positionally.
type UnitEntry = (usize, String, String, String);

/// Raw entry with its source path for re-expanding across drop-in merges.
type SourcedEntry = (usize, String, String, String, Option<String>);

/// Parse physical content into ordered (line, section, key, raw_value) entries,
/// handling line continuations and collecting syntax diagnostics.
fn unit_entries(content: &str, path: Option<&str>) -> (Vec<UnitEntry>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut logical_lines = Vec::new();
    let mut buffer = String::new();
    let mut start_line = 1;

    for (idx, physical) in content.lines().enumerate() {
        let line_num = idx + 1;
        let trimmed = physical.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if let Some(without_slash) = trimmed.strip_suffix('\\') {
            let without_slash = without_slash.trim_end();
            if buffer.is_empty() {
                start_line = line_num;
                buffer.push_str(without_slash);
            } else {
                buffer.push(' ');
                buffer.push_str(without_slash);
            }
        } else {
            if !buffer.is_empty() {
                buffer.push(' ');
                buffer.push_str(trimmed);
                logical_lines.push((start_line, std::mem::take(&mut buffer)));
            } else if !trimmed.is_empty() {
                logical_lines.push((line_num, trimmed.to_string()));
            }
        }
    }

    if !buffer.is_empty() {
        logical_lines.push((start_line, buffer));
    }

    let mut entries = Vec::new();
    let mut current_section = String::new();

    for (line_num, line) in logical_lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            let inner = &trimmed[1..trimmed.len() - 1];
            if inner.trim().is_empty() || inner.contains('[') || inner.contains(']') {
                diagnostics.push(Diagnostic {
                    code: "SYNTAX_SECTION_HEADER".to_string(),
                    severity: "warning".to_string(),
                    message: format!("Malformed section header '{}'", trimmed),
                    path: path.map(String::from),
                    line: Some(line_num),
                });
                continue;
            }
            current_section = inner.trim().to_string();
            continue;
        }
        if trimmed.starts_with('[') {
            // e.g. `[Unit` — a section header that was never closed.
            diagnostics.push(Diagnostic {
                code: "SYNTAX_SECTION_HEADER".to_string(),
                severity: "warning".to_string(),
                message: format!("Unclosed or malformed section header '{}'", trimmed),
                path: path.map(String::from),
                line: Some(line_num),
            });
            continue;
        }

        if let Some(eq_idx) = trimmed.find('=') {
            let key = trimmed[..eq_idx].trim().to_string();
            let raw_val = trimmed[eq_idx + 1..].trim().to_string();

            if current_section.is_empty() {
                diagnostics.push(Diagnostic {
                    code: "SYNTAX_KEY_BEFORE_SECTION".to_string(),
                    severity: "warning".to_string(),
                    message: format!("Key '{}' specified before any section header", key),
                    path: path.map(String::from),
                    line: Some(line_num),
                });
                continue;
            }

            entries.push((line_num, current_section.clone(), key, raw_val));
        } else {
            diagnostics.push(Diagnostic {
                code: "SYNTAX_GARBAGE_LINE".to_string(),
                severity: "warning".to_string(),
                message: format!("Non-assignment line ignored: '{}'", trimmed),
                path: path.map(String::from),
                line: Some(line_num),
            });
        }
    }

    (entries, diagnostics)
}

fn is_dependency_key(key: &str) -> bool {
    matches!(
        key,
        "Wants"
            | "Requires"
            | "Before"
            | "After"
            | "Requisite"
            | "BindsTo"
            | "PartOf"
            | "Conflicts"
            | "Upholds"
    )
}

/// Keep template specifiers until an actual instance name is known. This
/// makes template snapshots and independent parses self-contained.
fn expand_for_unit(value: &str, name: &str, manager: &ManagerContext) -> String {
    if is_template_name(name) {
        value.to_string()
    } else {
        expand_specifiers_with_manager(value, name, manager)
    }
}

/// Apply one raw assignment to `sections`, honoring systemd `Key=` reset
/// semantics: empty non-dependency assignments clear accumulated values.
fn apply_entry(
    sections: &mut BTreeMap<String, BTreeMap<String, Vec<String>>>,
    section: &str,
    key: &str,
    value: &str,
) {
    let sec_map = sections.entry(section.to_string()).or_default();
    if value.is_empty() {
        if section != "Unit" || !is_dependency_key(key) {
            sec_map.remove(key);
        }
    } else {
        sec_map
            .entry(key.to_string())
            .or_default()
            .push(value.to_string());
    }
}

/// Refresh the flattened dependency/exec fields from `sections`.
fn refresh_derived(unit: &mut SystemdUnit) {
    let mut wants = Vec::new();
    let mut requires = Vec::new();
    let mut before = Vec::new();
    let mut after = Vec::new();

    if let Some(sec) = unit.sections.get("Unit") {
        for (key, out) in [
            ("Wants", &mut wants),
            ("Requires", &mut requires),
            ("Before", &mut before),
            ("After", &mut after),
        ] {
            if let Some(lines) = sec.get(key) {
                for line in lines {
                    out.extend(line.split_whitespace().map(str::to_string));
                }
            }
        }
    }

    unit.exec_start = unit
        .sections
        .get("Service")
        .and_then(|s| s.get("ExecStart"))
        .and_then(|es| es.last())
        .cloned();

    wants.sort();
    wants.dedup();
    requires.sort();
    requires.dedup();
    before.sort();
    before.dedup();
    after.sort();
    after.dedup();

    unit.wants = wants;
    unit.requires = requires;
    unit.before = before;
    unit.after = after;
}

type SectionMap = BTreeMap<String, BTreeMap<String, Vec<String>>>;

fn expand_raw_entries(
    unit_name: &str,
    entries: &[SourcedEntry],
    manager: &ManagerContext,
) -> (SectionMap, Vec<OrderingEdge>) {
    let mut raw_sections: SectionMap = BTreeMap::new();
    let mut section_order = Vec::new();
    for (_, sec, _, _, _) in entries {
        if !raw_sections.contains_key(sec) {
            raw_sections.insert(sec.clone(), BTreeMap::new());
            section_order.push(sec.clone());
        }
    }
    for (_, sec, key, val, _) in entries {
        apply_entry(&mut raw_sections, sec, key, val);
    }

    // Service.User does not change the system-manager expansion context.
    let mut sections: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for sec_name in section_order {
        let raw_map = &raw_sections[&sec_name];
        let mut out_map = BTreeMap::new();
        for (k, vals) in raw_map {
            out_map.insert(
                k.clone(),
                vals.iter()
                    .map(|v| expand_for_unit(v, unit_name, manager))
                    .collect(),
            );
        }
        sections.insert(sec_name, out_map);
    }

    let mut ordering_edges = Vec::new();
    for (line, sec, key, raw_val, entry_path) in entries {
        if sec != "Unit" || (key != "After" && key != "Before") {
            continue;
        }
        let value = expand_for_unit(raw_val, unit_name, manager);
        if !value.is_empty() {
            for target in value.split_whitespace() {
                ordering_edges.push(OrderingEdge {
                    directive: key.clone(),
                    source: unit_name.to_string(),
                    target: target.to_string(),
                    path: entry_path.clone(),
                    line: Some(*line),
                });
            }
        }
    }

    (sections, ordering_edges)
}

pub(crate) fn instantiate_template(tpl: &SystemdUnit, instance_name: &str) -> SystemdUnit {
    instantiate_template_with_manager(tpl, instance_name, &ManagerContext::default())
}

pub(crate) fn instantiate_template_with_manager(
    tpl: &SystemdUnit,
    instance_name: &str,
    manager: &ManagerContext,
) -> SystemdUnit {
    let mut inst = tpl.clone();
    inst.name = instance_name.to_string();
    for section in inst.sections.values_mut() {
        for values in section.values_mut() {
            for value in values {
                *value = expand_specifiers_with_manager(value, instance_name, manager);
            }
        }
    }
    for edge in &mut inst.ordering_edges {
        edge.source = instance_name.to_string();
        edge.target = expand_specifiers_with_manager(&edge.target, instance_name, manager);
    }
    for dependencies in [
        &mut inst.before,
        &mut inst.after,
        &mut inst.wants,
        &mut inst.requires,
    ] {
        for target in dependencies {
            *target = expand_specifiers_with_manager(target, instance_name, manager);
        }
    }
    inst.exec_start = inst
        .exec_start
        .map(|s| expand_specifiers_with_manager(&s, instance_name, manager));
    inst
}

pub fn parse_unit_content(content: &str, unit_name: &str, path: Option<&str>) -> SystemdUnit {
    parse_unit_content_with_manager(content, unit_name, path, &ManagerContext::default())
}

pub fn parse_unit_content_with_manager(
    content: &str,
    unit_name: &str,
    path: Option<&str>,
    manager: &ManagerContext,
) -> SystemdUnit {
    let (entries, diagnostics) = unit_entries(content, path);
    let sourced: Vec<SourcedEntry> = entries
        .into_iter()
        .map(|(line, sec, key, val)| (line, sec, key, val, path.map(String::from)))
        .collect();

    let (sections, ordering_edges) = expand_raw_entries(unit_name, &sourced, manager);

    let mut unit = SystemdUnit {
        name: unit_name.to_string(),
        path: path.map(String::from),
        sections,
        wants: Vec::new(),
        requires: Vec::new(),
        before: Vec::new(),
        after: Vec::new(),
        exec_start: None,
        drop_ins: Vec::new(),
        ordering_edges,
        masked: false,
        alias_of: None,
        aliases: Vec::new(),
        diagnostics,
    };
    refresh_derived(&mut unit);
    unit
}

/// Merge a drop-in fragment. Non-dependency settings support empty resets;
/// dependencies are additive, including empty Before=/After= assignments.
pub fn apply_drop_in(base: &mut SystemdUnit, drop_in_content: &str, drop_in_path: &str) {
    apply_drop_in_with_manager(
        base,
        drop_in_content,
        drop_in_path,
        &ManagerContext::default(),
    )
}

pub fn apply_drop_in_with_manager(
    base: &mut SystemdUnit,
    drop_in_content: &str,
    drop_in_path: &str,
    manager: &ManagerContext,
) {
    base.drop_ins.push(drop_in_path.to_string());
    let (entries, diagnostics) = unit_entries(drop_in_content, Some(drop_in_path));
    base.diagnostics.extend(diagnostics);
    for (line, sec, key, raw_val) in entries {
        let value = expand_for_unit(&raw_val, &base.name, manager);
        if sec == "Unit" && (key == "After" || key == "Before") {
            for target in value.split_whitespace() {
                base.ordering_edges.push(OrderingEdge {
                    directive: key.clone(),
                    source: base.name.clone(),
                    target: target.to_string(),
                    path: Some(drop_in_path.to_string()),
                    line: Some(line),
                });
            }
        }
        apply_entry(&mut base.sections, &sec, &key, &value);
    }
    refresh_derived(base);
}
