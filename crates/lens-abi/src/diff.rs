use lens_core::{Compatibility, SetDiff};
use std::collections::HashMap;

use crate::model::*;

/// Serialize an `InputStatus` with the same kebab-case vocabulary
/// `ElfReport.status` uses ("non-elf", "tool-error", ...).
fn status_string(status: &InputStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", status).to_lowercase())
}

pub fn diff_reports(left: &ElfReport, right: &ElfReport) -> DiffReport {
    let mut header_changes = Vec::new();

    if left.elf.class != right.elf.class {
        header_changes.push(format!(
            "ELF class: {} -> {}",
            left.elf.class, right.elf.class
        ));
    }
    if left.elf.endian != right.elf.endian {
        header_changes.push(format!(
            "endianness: {} -> {}",
            left.elf.endian, right.elf.endian
        ));
    }
    if left.elf.machine != right.elf.machine {
        header_changes.push(format!(
            "machine: {} -> {}",
            left.elf.machine, right.elf.machine
        ));
    }
    if left.elf.elf_type != right.elf.elf_type {
        header_changes.push(format!(
            "type: {} -> {}",
            left.elf.elf_type, right.elf.elf_type
        ));
    }
    if left.elf.dynamic != right.elf.dynamic {
        header_changes.push("linkage: dynamic/static changed".to_string());
    }
    if left.dependencies.soname != right.dependencies.soname {
        header_changes.push(format!(
            "soname: {} -> {}",
            left.dependencies.soname.as_deref().unwrap_or("<none>"),
            right.dependencies.soname.as_deref().unwrap_or("<none>")
        ));
    }

    let needed_diff = SetDiff::compute(
        left.dependencies.needed.clone(),
        right.dependencies.needed.clone(),
    );
    let rpath_diff = SetDiff::compute(
        left.dependencies.rpath.clone(),
        right.dependencies.rpath.clone(),
    );
    let runpath_diff = SetDiff::compute(
        left.dependencies.runpath.clone(),
        right.dependencies.runpath.clone(),
    );

    let symbols_diff = SetDiff::compute(left.abi.symbols.clone(), right.abi.symbols.clone());
    let imports_diff = SetDiff::compute(left.abi.imports.clone(), right.abi.imports.clone());
    let vtables_diff = SetDiff::compute(left.abi.vtables.clone(), right.abi.vtables.clone());
    let types_diff = SetDiff::compute(left.abi.types.clone(), right.abi.types.clone());
    // Diff symbol-version requirements (GLIBC_2.x etc.) — a new requirement
    // is a real forward-compat break, so it must be visible.
    let version_key = |v: &VersionRequirement| {
        if v.library.is_empty() {
            format!("{}:{}", v.namespace, v.version)
        } else {
            format!("{}:{}", v.library, v.version)
        }
    };
    let abi_diff = SetDiff::compute(
        left.abi
            .versions
            .iter()
            .map(version_key)
            .collect::<Vec<String>>(),
        right
            .abi
            .versions
            .iter()
            .map(version_key)
            .collect::<Vec<String>>(),
    );

    // Symbol attributes diff (size, type, binding) — compare only defined
    // (exported) symbols on both sides so an undefined import becoming a
    // defined export is classified as Added (Compatible), not a type change.
    let left_evidence_map: HashMap<&str, &SymbolEvidence> = left
        .evidence
        .iter()
        .filter(|s| s.defined)
        .map(|s| (s.identity.as_str(), s))
        .collect();

    let mut symbol_changes = Vec::new();
    let mut breaking_attribute = false;

    for r_sym in right.evidence.iter().filter(|s| s.defined) {
        if let Some(l_sym) = left_evidence_map.get(r_sym.identity.as_str()) {
            let mut diffs = Vec::new();
            if l_sym.symbol_type != r_sym.symbol_type {
                diffs.push("type");
                breaking_attribute = true;
            }
            if l_sym.binding != r_sym.binding {
                diffs.push("binding");
            }
            // If data/TLS object size changed, ABI layout is broken
            if l_sym.size != r_sym.size
                && (r_sym.symbol_type == "OBJECT"
                    || r_sym.symbol_type == "COMMON"
                    || r_sym.symbol_type == "TLS")
            {
                diffs.push("size");
                breaking_attribute = true;
            }

            if !diffs.is_empty() {
                symbol_changes.push(format!("{}: {}", r_sym.identity, diffs.join(" ")));
            }
        }
    }

    // Check DWARF type byte-size changes (`TypeName (size=N)`).
    fn parse_type_size(entry: &str) -> Option<(&str, &str)> {
        let (name, rest) = entry.rsplit_once(" (size=")?;
        let size = rest.strip_suffix(')')?;
        Some((name, size))
    }
    let left_type_sizes: HashMap<&str, &str> = left
        .abi
        .types
        .iter()
        .filter_map(|t| parse_type_size(t))
        .collect();
    let mut breaking_type_layout = false;
    for r_type in &right.abi.types {
        if let Some((name, r_sz)) = parse_type_size(r_type) {
            if let Some(&l_sz) = left_type_sizes.get(name) {
                if l_sz != r_sz {
                    breaking_type_layout = true;
                }
            }
        }
    }

    let both_valid = left.status == InputStatus::Valid && right.status == InputStatus::Valid;
    let mut diagnostics = Vec::new();

    // 3-state compatibility evaluation
    let mut incompatible = false;
    let mut uncertain = false;

    if !both_valid {
        // Invalid input is not evidence of incompatibility — it is
        // missing evidence, so the verdict is uncertain.
        uncertain = true;
        diagnostics.push("One or both inputs are invalid ELF files".to_string());
    } else {
        if !header_changes.is_empty() {
            incompatible = true;
        }
        if !symbols_diff.removed.is_empty() {
            incompatible = true;
            diagnostics.push(format!(
                "Exported symbols removed: {}",
                symbols_diff.removed.len()
            ));
        }
        if !vtables_diff.removed.is_empty() {
            incompatible = true;
            diagnostics.push(format!(
                "Virtual tables removed: {}",
                vtables_diff.removed.len()
            ));
        }
        if breaking_attribute || breaking_type_layout {
            incompatible = true;
        }
        if needed_diff.has_changed() || rpath_diff.has_changed() || runpath_diff.has_changed() {
            uncertain = true;
            diagnostics.push("Dynamic loader search paths or dependencies modified".to_string());
        }
        if imports_diff.has_changed() {
            diagnostics.push(format!(
                "Imported symbols changed (+{} -{})",
                imports_diff.added.len(),
                imports_diff.removed.len()
            ));
        }
        if abi_diff.has_changed() {
            if abi_diff.removed.iter().any(|v| v.starts_with("self:")) {
                incompatible = true;
            } else {
                uncertain = true;
            }
            diagnostics.push(format!(
                "Symbol version requirements changed (+{} -{})",
                abi_diff.added.len(),
                abi_diff.removed.len()
            ));
        }
    }

    let compatibility = if incompatible {
        Compatibility::Incompatible
    } else if uncertain {
        Compatibility::Uncertain
    } else {
        Compatibility::Compatible
    };

    let changed = !header_changes.is_empty()
        || needed_diff.has_changed()
        || rpath_diff.has_changed()
        || runpath_diff.has_changed()
        || symbols_diff.has_changed()
        || imports_diff.has_changed()
        || vtables_diff.has_changed()
        || abi_diff.has_changed()
        || types_diff.has_changed()
        || !symbol_changes.is_empty();

    DiffReport {
        schema: DIFF_SCHEMA_V3.to_string(),
        left: left.input.clone(),
        right: right.input.clone(),
        changed,
        compatible: compatibility.is_compatible(),
        compatibility,
        left_status: status_string(&left.status),
        right_status: status_string(&right.status),
        header_changes,
        dependencies: DiffDependencies {
            needed: needed_diff,
            rpath: rpath_diff,
            runpath: runpath_diff,
        },
        symbols: symbols_diff,
        imports: imports_diff,
        vtables: vtables_diff,
        abi: abi_diff,
        types: types_diff,
        symbol_changes,
        diagnostics,
    }
}
