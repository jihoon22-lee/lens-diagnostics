//! Minimal DWARF `.debug_info` extraction: collects the names of declared
//! types (structs, classes, unions, enums, typedefs) so `abi.types` is real
//! evidence instead of an empty placeholder. Bounded and best-effort —
//! stripped binaries simply yield no types and a diagnostic note.

use gimli::Reader;
use object::{Object, ObjectSection};
use std::collections::BTreeMap;
use std::rc::Rc;

/// Upper bound on extracted type names to keep reports bounded.
const MAX_TYPES: usize = 10_000;

type GimliReader = gimli::EndianRcSlice<gimli::RunTimeEndian>;

/// Extract declared type names from a parsed object file.
/// Returns `(types, diagnostic)`.
pub fn extract_dwarf_types(file: &object::File) -> (Vec<String>, Option<String>) {
    let has_debug_info = file
        .section_by_name(".debug_info")
        .map(|s| s.size() > 0)
        .unwrap_or(false)
        || file
            .section_by_name(".zdebug_info")
            .map(|s| s.size() > 0)
            .unwrap_or(false);

    if !has_debug_info {
        return (
            Vec::new(),
            Some("no .debug_info section (stripped or built without -g)".to_string()),
        );
    }

    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    let dwarf = gimli::Dwarf::load(|id| -> Result<GimliReader, gimli::Error> {
        // uncompressed_data() handles SHF_COMPRESSED and .zdebug_* payloads;
        // the zdebug fallback covers sections renamed by objcopy.
        let zname = format!(".z{}", &id.name()[1..]);
        let data: Vec<u8> = file
            .section_by_name(id.name())
            .or_else(|| file.section_by_name(&zname))
            .and_then(|s| s.uncompressed_data().ok())
            .map(|c| c.into_owned())
            .unwrap_or_default();
        Ok(gimli::EndianRcSlice::new(Rc::from(data), endian))
    });

    let dwarf = match dwarf {
        Ok(d) => d,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("failed to load DWARF sections: {}", e)),
            )
        }
    };

    let mut types: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let mut units = dwarf.units();
    let mut truncated = false;
    let mut parse_error = false;

    loop {
        let header = match units.next() {
            Ok(Some(h)) => h,
            Ok(None) => break,
            Err(_) => {
                parse_error = true;
                break;
            }
        };
        let unit = match dwarf.unit(header) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let mut entries = unit.entries();
        loop {
            match entries.next_entry() {
                Ok(Some(())) => {
                    let entry = match entries.current() {
                        Some(e) => e,
                        None => continue,
                    };
                    if matches!(
                        entry.tag(),
                        gimli::DW_TAG_structure_type
                            | gimli::DW_TAG_class_type
                            | gimli::DW_TAG_union_type
                            | gimli::DW_TAG_enumeration_type
                            | gimli::DW_TAG_typedef
                    ) {
                        if let Some(attr) = entry.attr(gimli::DW_AT_name).ok().flatten() {
                            if let Ok(reader) = dwarf.attr_string(&unit, attr.value()) {
                                if let Ok(name) = reader.to_string() {
                                    if !name.is_empty() {
                                        let byte_size = entry
                                            .attr(gimli::DW_AT_byte_size)
                                            .ok()
                                            .flatten()
                                            .and_then(|a| a.udata_value());
                                        let name_owned = name.into_owned();
                                        let slot = types.entry(name_owned).or_insert(byte_size);
                                        if slot.is_none() && byte_size.is_some() {
                                            *slot = byte_size;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if types.len() >= MAX_TYPES {
                        truncated = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        if truncated {
            break;
        }
    }

    let diagnostic = if truncated {
        Some(format!("type list truncated at {} entries", MAX_TYPES))
    } else if parse_error {
        Some("DWARF unit iteration failed; type list may be incomplete".to_string())
    } else if types.is_empty() {
        Some(".debug_info present but contained no named types".to_string())
    } else {
        None
    };

    let formatted = types
        .into_iter()
        .map(|(name, sz)| match sz {
            Some(s) => format!("{name} (size={s})"),
            None => name,
        })
        .collect();

    (formatted, diagnostic)
}
