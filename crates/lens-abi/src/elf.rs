use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};
use std::path::Path;

use crate::dwarf::extract_dwarf_types;
use crate::model::*;

/// Parsed ELF_DYNAMIC payload: loader search paths, SONAME, interpreter, and
/// symbol version definitions/requirements.
#[derive(Default)]
struct DynamicInfo {
    rpath: Vec<String>,
    runpath: Vec<String>,
    soname: Option<String>,
    interpreter: Option<String>,
    /// (library, version) pairs from .gnu.version_r / .gnu.version_d
    versions: Vec<VersionRequirement>,
}

const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;
const DT_SONAME: i64 = 14;
const DT_RPATH: i64 = 15;
const DT_RUNPATH: i64 = 29;
const DT_VERDEF: i64 = 0x6ffffffc;
const DT_VERDEFNUM: i64 = 0x6ffffffd;
const DT_VERNEED: i64 = 0x6ffffffe;
const DT_VERNEEDNUM: i64 = 0x6fffffff;

const PT_LOAD: u64 = 1;
const PT_DYNAMIC: u64 = 2;
const PT_INTERP: u64 = 3;
const SHF_ALLOC: u64 = 0x2;

fn rd_u16(d: &[u8], off: usize, le: bool) -> Option<u64> {
    let end = off.checked_add(2)?;
    let b: [u8; 2] = d.get(off..end)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(b) as u64
    } else {
        u16::from_be_bytes(b) as u64
    })
}

fn rd_u32(d: &[u8], off: usize, le: bool) -> Option<u64> {
    let end = off.checked_add(4)?;
    let b: [u8; 4] = d.get(off..end)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(b) as u64
    } else {
        u32::from_be_bytes(b) as u64
    })
}

fn rd_dyn(d: &[u8], off: usize, is64: bool, le: bool) -> Option<(i64, u64)> {
    if is64 {
        let mid = off.checked_add(8)?;
        let end = off.checked_add(16)?;
        let tag: [u8; 8] = d.get(off..mid)?.try_into().ok()?;
        let val: [u8; 8] = d.get(mid..end)?.try_into().ok()?;
        let (t, v) = if le {
            (i64::from_le_bytes(tag), u64::from_le_bytes(val))
        } else {
            (i64::from_be_bytes(tag), u64::from_be_bytes(val))
        };
        Some((t, v))
    } else {
        let mid = off.checked_add(4)?;
        let end = off.checked_add(8)?;
        let tag: [u8; 4] = d.get(off..mid)?.try_into().ok()?;
        let val: [u8; 4] = d.get(mid..end)?.try_into().ok()?;
        let (t, v) = if le {
            (
                i32::from_le_bytes(tag) as i64,
                u32::from_le_bytes(val) as u64,
            )
        } else {
            (
                i32::from_be_bytes(tag) as i64,
                u32::from_be_bytes(val) as u64,
            )
        };
        Some((t, v))
    }
}

/// A PT_LOAD segment for vaddr→file-offset mapping.
struct LoadSegment {
    vaddr: u64,
    offset: u64,
    filesz: u64,
}

/// Program headers decoded straight from the ELF bytes — still available
/// when section headers were stripped (`sstrip`, UPX, corrupted binaries).
struct ProgramHeaders {
    loads: Vec<LoadSegment>,
    /// (file_offset, file_size) of PT_DYNAMIC
    dynamic: Option<(u64, u64)>,
    /// (file_offset, file_size) of PT_INTERP
    interp: Option<(u64, u64)>,
}

fn parse_program_headers(data: &[u8], is64: bool, le: bool) -> ProgramHeaders {
    let mut ph = ProgramHeaders {
        loads: Vec::new(),
        dynamic: None,
        interp: None,
    };
    let read32 = |off: usize| -> Option<u64> {
        let end = off.checked_add(4)?;
        let b: [u8; 4] = data.get(off..end)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b) as u64
        } else {
            u32::from_be_bytes(b) as u64
        })
    };
    let read64 = |off: usize| -> Option<u64> {
        let end = off.checked_add(8)?;
        let b: [u8; 8] = data.get(off..end)?.try_into().ok()?;
        Some(if le {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    };
    // ELF header: e_phoff, e_phentsize, e_phnum
    let (phoff, phentsize, phnum) = if is64 {
        (
            read64(32).and_then(|v| usize::try_from(v).ok()),
            rd_u16(data, 54, le).and_then(|v| usize::try_from(v).ok()),
            rd_u16(data, 56, le).and_then(|v| usize::try_from(v).ok()),
        )
    } else {
        (
            read32(28).and_then(|v| usize::try_from(v).ok()),
            rd_u16(data, 42, le).and_then(|v| usize::try_from(v).ok()),
            rd_u16(data, 44, le).and_then(|v| usize::try_from(v).ok()),
        )
    };
    let (Some(phoff), Some(phentsize), Some(phnum)) = (phoff, phentsize, phnum) else {
        return ph;
    };
    if phentsize == 0 {
        return ph;
    }

    for i in 0..phnum.min(4096) {
        let Some(base) = i
            .checked_mul(phentsize)
            .and_then(|off| phoff.checked_add(off))
        else {
            break;
        };
        let (p_type, p_offset, p_vaddr, p_filesz) = if is64 {
            (
                read32(base),
                base.checked_add(8).and_then(read64),
                base.checked_add(16).and_then(read64),
                base.checked_add(32).and_then(read64),
            )
        } else {
            (
                read32(base),
                base.checked_add(4).and_then(read32),
                base.checked_add(8).and_then(read32),
                base.checked_add(16).and_then(read32),
            )
        };
        let (Some(t), Some(off), Some(vaddr), Some(filesz)) = (p_type, p_offset, p_vaddr, p_filesz)
        else {
            continue;
        };
        match t {
            PT_LOAD => ph.loads.push(LoadSegment {
                vaddr,
                offset: off,
                filesz,
            }),
            PT_DYNAMIC => ph.dynamic = Some((off, filesz)),
            PT_INTERP => ph.interp = Some((off, filesz)),
            _ => {}
        }
    }
    ph
}

/// True if the section is SHF_ALLOC (occupies the runtime address space);
/// non-ELF objects are treated as alloc so the fallback stays usable.
fn section_is_alloc(s: &object::Section) -> bool {
    match s.flags() {
        object::SectionFlags::Elf { sh_flags } => sh_flags & SHF_ALLOC != 0,
        _ => true,
    }
}

/// Map a virtual address into file bytes. Dynamic tags always point into
/// PT_LOAD segments; sections are the fallback for files without program
/// headers (e.g. relocatable objects), restricted to SHF_ALLOC sections so
/// non-alloc sections can't shadow runtime addresses.
fn vaddr_bytes<'a>(
    file: &'a object::File<'a>,
    data: &'a [u8],
    ph: &ProgramHeaders,
    vaddr: u64,
) -> Option<&'a [u8]> {
    for seg in &ph.loads {
        if vaddr >= seg.vaddr && vaddr < seg.vaddr.saturating_add(seg.filesz) {
            let file_off = seg.offset.checked_add(vaddr - seg.vaddr)?;
            let off = usize::try_from(file_off).ok()?;
            if off <= data.len() {
                return Some(&data[off..]);
            }
        }
    }
    for s in file.sections() {
        if !section_is_alloc(&s) {
            continue;
        }
        let start = s.address();
        let end = start.saturating_add(s.size());
        if s.size() > 0 && vaddr >= start && vaddr < end {
            if let Ok(d) = s.data() {
                let off = usize::try_from(vaddr - start).ok()?;
                if off <= d.len() {
                    return Some(&d[off..]);
                }
            }
        }
    }
    None
}

fn cstr_at(d: &[u8], off: usize) -> Option<String> {
    let end = d
        .get(off..)?
        .iter()
        .position(|&b| b == 0)?
        .checked_add(off)?;
    Some(String::from_utf8_lossy(&d[off..end]).into_owned())
}

fn parse_verneed<'a>(
    map: &dyn Fn(u64) -> Option<&'a [u8]>,
    base_vaddr: u64,
    count: u64,
    strtab: &[u8],
    le: bool,
    out: &mut Vec<VersionRequirement>,
) {
    // Elf*_Verneed: vn_version@0 u16, vn_cnt@2 u16, vn_file@4 u32,
    //               vn_aux@8 u32, vn_next@12 u32  (16 bytes)
    let mut cur = base_vaddr;
    for _ in 0..count.min(1024) {
        let Some(ent) = map(cur) else {
            break;
        };
        let Some(cnt) = rd_u16(ent, 2, le) else { break };
        let Some(file_off) = rd_u32(ent, 4, le) else {
            break;
        };
        let Some(aux) = rd_u32(ent, 8, le) else { break };
        let Some(next) = rd_u32(ent, 12, le) else {
            break;
        };
        let library = cstr_at(strtab, file_off as usize).unwrap_or_default();

        // Vernaux follows at cur + vn_aux; chain via vna_next.
        let mut aux_cur = cur.saturating_add(aux);
        for _ in 0..cnt.min(1024) {
            let Some(a) = map(aux_cur) else {
                break;
            };
            let Some(name_off) = rd_u32(a, 8, le) else {
                break;
            };
            let Some(anext) = rd_u32(a, 12, le) else {
                break;
            };
            if let Some(version) = cstr_at(strtab, name_off as usize) {
                out.push(VersionRequirement {
                    library: library.clone(),
                    namespace: library.clone(),
                    version,
                });
            }
            if anext == 0 {
                break;
            }
            aux_cur = aux_cur.saturating_add(anext);
        }

        if next == 0 {
            break;
        }
        cur = cur.saturating_add(next);
    }
}

fn parse_verdef<'a>(
    map: &dyn Fn(u64) -> Option<&'a [u8]>,
    base_vaddr: u64,
    count: u64,
    strtab: &[u8],
    le: bool,
    out: &mut Vec<VersionRequirement>,
) {
    // Elf*_Verdef: vd_version@0, vd_flags@2, vd_ndx@4, vd_cnt@6 u16,
    //              vd_hash@8, vd_aux@12, vd_next@16 u32  (20 bytes)
    let mut cur = base_vaddr;
    for _ in 0..count.min(1024) {
        let Some(ent) = map(cur) else {
            break;
        };
        let Some(flags) = rd_u16(ent, 2, le) else {
            break;
        };
        let Some(cnt) = rd_u16(ent, 6, le) else { break };
        let Some(aux) = rd_u32(ent, 12, le) else {
            break;
        };
        let Some(next) = rd_u32(ent, 16, le) else {
            break;
        };

        // Skip VER_FLG_BASE (0x1) which names the file itself, and read only
        // the first Verdaux entry (subsequent Verdaux entries are parent predecessors).
        if (flags & 1) == 0 && cnt > 0 {
            let aux_cur = cur.saturating_add(aux);
            if let Some(a) = map(aux_cur) {
                if let Some(name_off) = rd_u32(a, 0, le) {
                    if let Some(version) = cstr_at(strtab, name_off as usize) {
                        out.push(VersionRequirement {
                            library: String::new(),
                            namespace: "self".to_string(),
                            version,
                        });
                    }
                }
            }
        }

        if next == 0 {
            break;
        }
        cur = cur.saturating_add(next);
    }
}

fn slice_from_u64_range(data: &[u8], off: u64, size: u64) -> Option<&[u8]> {
    let start = usize::try_from(off).ok()?;
    let len = usize::try_from(size).ok()?;
    let end = start.checked_add(len)?;
    data.get(start..end)
}

fn parse_dynamic(file: &object::File, data: &[u8]) -> DynamicInfo {
    let le = file.is_little_endian();
    let is64 = file.is_64();
    let ph = parse_program_headers(data, is64, le);
    let map = |v: u64| vaddr_bytes(file, data, &ph, v);

    let interp_bytes = ph
        .interp
        .and_then(|(off, size)| slice_from_u64_range(data, off, size))
        .or_else(|| file.section_by_name(".interp").and_then(|s| s.data().ok()));
    let mut info = DynamicInfo {
        interpreter: interp_bytes.and_then(|d| cstr_at(d, 0)),
        ..Default::default()
    };

    // .dynamic via PT_DYNAMIC first — section headers may be stripped.
    let dyn_data: &[u8] = ph
        .dynamic
        .and_then(|(off, size)| slice_from_u64_range(data, off, size))
        .or_else(|| file.section_by_name(".dynamic").and_then(|s| s.data().ok()))
        .unwrap_or(&[]);
    if dyn_data.is_empty() {
        return info;
    }

    let entry_size = if is64 { 16 } else { 8 };
    let mut strtab_vaddr = None;
    let mut strtab_size = usize::MAX;
    let mut rpath_off = None;
    let mut runpath_off = None;
    let mut soname_off = None;
    let mut verneed = None;
    let mut verneednum = 0u64;
    let mut verdef = None;
    let mut verdefnum = 0u64;

    for off in (0..dyn_data.len()).step_by(entry_size) {
        let Some((tag, val)) = rd_dyn(dyn_data, off, is64, le) else {
            break;
        };
        match tag {
            DT_STRTAB => strtab_vaddr = Some(val),
            DT_STRSZ => strtab_size = val as usize,
            DT_SONAME => soname_off = Some(val),
            DT_RPATH => rpath_off = Some(val),
            DT_RUNPATH => runpath_off = Some(val),
            DT_VERNEED => verneed = Some(val),
            DT_VERNEEDNUM => verneednum = val,
            DT_VERDEF => verdef = Some(val),
            DT_VERDEFNUM => verdefnum = val,
            0 => break, // DT_NULL
            _ => {}
        }
    }

    let strtab: &[u8] = strtab_vaddr
        .and_then(map)
        .map(|d| &d[..d.len().min(strtab_size)])
        .or_else(|| file.section_by_name(".dynstr").and_then(|s| s.data().ok()))
        .unwrap_or(&[]);

    if let Some(off) = soname_off {
        info.soname = cstr_at(strtab, off as usize);
    }
    if let Some(off) = rpath_off {
        info.rpath = cstr_at(strtab, off as usize)
            .map(|s| s.split(':').map(|x| x.to_string()).collect())
            .unwrap_or_default();
    }
    if let Some(off) = runpath_off {
        info.runpath = cstr_at(strtab, off as usize)
            .map(|s| s.split(':').map(|x| x.to_string()).collect())
            .unwrap_or_default();
    }

    if let (Some(v), n) = (verneed, verneednum) {
        parse_verneed(&map, v, n, strtab, le, &mut info.versions);
    }
    if let (Some(v), n) = (verdef, verdefnum) {
        parse_verdef(&map, v, n, strtab, le, &mut info.versions);
    }
    info.versions.sort_by(|a, b| {
        (&a.library, &a.namespace, &a.version).cmp(&(&b.library, &b.namespace, &b.version))
    });
    info.versions.dedup();
    info
}

pub fn demangle_symbol(name: &str) -> Option<String> {
    let raw_name = name.split_once('@').map(|(s, _)| s).unwrap_or(name);

    if let Ok(sym) = cpp_demangle::Symbol::new(raw_name) {
        if let Ok(demangled) = sym.demangle(&cpp_demangle::DemangleOptions::default()) {
            return Some(demangled);
        }
    }

    let rust_demangled = rustc_demangle::demangle(raw_name).to_string();
    if rust_demangled != raw_name {
        Some(rust_demangled)
    } else {
        None
    }
}

fn tool_info() -> ToolInfo {
    ToolInfo {
        name: "abilens".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn invalid_report(
    input_path: String,
    status: InputStatus,
    message: String,
    diagnostics: Vec<String>,
) -> ElfReport {
    ElfReport {
        schema: REPORT_SCHEMA_V2.to_string(),
        input: input_path,
        status,
        message,
        tool: tool_info(),
        elf: ElfHeaderInfo::default(),
        dependencies: Dependencies::default(),
        abi: AbiData::default(),
        policy: PolicyEvaluation::default(),
        diagnostics,
        evidence: Vec::new(),
    }
}

pub fn inspect_elf<P: AsRef<Path>>(path: P, data: &[u8]) -> ElfReport {
    let input_path = path.as_ref().to_string_lossy().into_owned();

    // Check ELF magic
    if data.len() < 4 || &data[0..4] != b"\x7fELF" {
        return invalid_report(
            input_path,
            InputStatus::NonElf,
            "file is not an ELF binary".to_string(),
            Vec::new(),
        );
    }

    let file = match object::File::parse(data) {
        Ok(f) => f,
        Err(e) => {
            return invalid_report(
                input_path,
                InputStatus::Corrupt,
                format!("ELF parsing failed: {}", e),
                vec![e.to_string()],
            );
        }
    };

    let is_64 = file.is_64();
    let class = if is_64 { "ELF64" } else { "ELF32" };
    let endian = if file.is_little_endian() {
        "little-endian"
    } else {
        "big-endian"
    };
    let machine = format!("{:?}", file.architecture());
    let elf_type = format!("{:?}", file.kind());

    let mut needed = Vec::new();
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut vtables = Vec::new();
    let mut evidence = Vec::new();

    // Check stripped status: .symtab present?
    let stripped = file.section_by_name(".symtab").is_none();

    // Dynamic symbols
    let mut dynamic = false;
    for sym in file.dynamic_symbols() {
        dynamic = true;
        if let Ok(name) = sym.name() {
            if name.is_empty() {
                continue;
            }

            let (st_info, st_other) = match sym.flags() {
                object::SymbolFlags::Elf { st_info, st_other } => (Some(st_info), Some(st_other)),
                _ => (None, None),
            };

            let visibility = match st_other.map(|o| o & 0x03).unwrap_or(0) {
                0 => "default",
                1 => "internal",
                2 => "hidden",
                3 => "protected",
                _ => "default",
            };

            // SymbolScope::Linkage is not ELF weakness — ask the symbol / st_info.
            let binding = if sym.is_weak() {
                "weak"
            } else if sym.is_local() || st_info.map(|i| (i >> 4) == 0).unwrap_or(false) {
                "local"
            } else {
                "global"
            };

            let st_type = st_info.map(|i| i & 0x0f);
            let symbol_type = if sym.kind() == SymbolKind::Tls || matches!(st_type, Some(6 | 7)) {
                "TLS"
            } else if st_type == Some(5) {
                "COMMON"
            } else {
                match sym.kind() {
                    SymbolKind::Text => "FUNC",
                    SymbolKind::Data => "OBJECT",
                    SymbolKind::Section => "SECTION",
                    SymbolKind::File => "FILE",
                    _ => "NOTYPE",
                }
            };

            // Undefined dynamic symbols are imports, not exports —
            // keep them out of `abi.symbols` so the diff never reports
            // a changed dependency as a removed export.
            // Defined symbols are exported only when binding is global/weak
            // and visibility is default/protected.
            let defined = !sym.is_undefined();
            let is_export_binding = binding == "global" || binding == "weak";
            let is_export_vis = visibility == "default" || visibility == "protected";
            let is_vtable = defined && name.starts_with("_ZTV");
            if !defined {
                imports.push(name.to_string());
            } else if is_export_binding && is_export_vis {
                if is_vtable {
                    vtables.push(name.to_string());
                } else {
                    symbols.push(name.to_string());
                }
            }

            evidence.push(SymbolEvidence {
                identity: name.to_string(),
                size: sym.size(),
                binding: binding.to_string(),
                visibility: visibility.to_string(),
                symbol_type: symbol_type.to_string(),
                default_version: name.contains("@@"),
                defined,
                demangled: demangle_symbol(name),
            });
        }
    }

    // Dynamic libraries (DT_NEEDED)
    for lib in file.imports().into_iter().flatten() {
        let name = String::from_utf8_lossy(lib.library()).to_string();
        if !name.is_empty() && !needed.contains(&name) {
            needed.push(name);
        }
    }

    // .dynamic payload: rpath/runpath/soname/interp + symbol versions
    let dyn_info = parse_dynamic(&file, data);

    // DWARF declared types (empty on stripped binaries — recorded below)
    let (types, dwarf_diag) = extract_dwarf_types(&file);
    let mut diagnostics = Vec::new();
    if let Some(d) = dwarf_diag {
        diagnostics.push(d);
    }

    symbols.sort();
    symbols.dedup();
    imports.sort();
    imports.dedup();
    vtables.sort();
    vtables.dedup();
    needed.sort();
    evidence.sort_by(|a, b| a.identity.cmp(&b.identity));

    ElfReport {
        schema: REPORT_SCHEMA_V2.to_string(),
        input: input_path,
        status: InputStatus::Valid,
        message: "ELF inspection completed successfully".to_string(),
        tool: tool_info(),
        elf: ElfHeaderInfo {
            class: class.to_string(),
            endian: endian.to_string(),
            machine,
            elf_type,
            dynamic,
            stripped,
        },
        dependencies: Dependencies {
            needed,
            rpath: dyn_info.rpath,
            runpath: dyn_info.runpath,
            soname: dyn_info.soname,
            interpreter: dyn_info.interpreter,
        },
        abi: AbiData {
            versions: dyn_info.versions,
            symbols,
            imports,
            vtables,
            types,
        },
        policy: PolicyEvaluation {
            applied: false,
            passed: true,
            violations: Vec::new(),
        },
        diagnostics,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command;

    /// Compile a tiny shared object; returns None when no C toolchain
    /// is available so the test self-skips instead of failing.
    fn compile_so(code: &str) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        let dir = tempfile::tempdir().ok()?;
        let src = dir.path().join("t.c");
        let so = dir.path().join("t.so");
        std::fs::File::create(&src)
            .ok()?
            .write_all(code.as_bytes())
            .ok()?;
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-g", "-o"])
            .arg(&so)
            .arg(&src)
            .status()
            .ok()?;
        status.success().then_some((dir, so))
    }

    #[test]
    fn test_imports_and_weak_binding() {
        let Some((_dir, so)) = compile_so(
            "extern int imported_fn(void);\n\
             __attribute__((weak)) int weak_fn(void) { return imported_fn(); }\n\
             int exported_fn(void) { return 42; }\n",
        ) else {
            return;
        };
        let data = std::fs::read(&so).unwrap();
        let report = inspect_elf(&so, &data);

        // Undefined (imported) symbols land in abi.imports, never in
        // the exported surface.
        assert!(report.abi.imports.contains(&"imported_fn".to_string()));
        assert!(!report.abi.symbols.contains(&"imported_fn".to_string()));
        assert!(report.abi.symbols.contains(&"exported_fn".to_string()));

        let weak = report
            .evidence
            .iter()
            .find(|s| s.identity == "weak_fn")
            .expect("weak_fn in evidence");
        assert_eq!(weak.binding, "weak");
        assert!(weak.defined);
        let imp = report
            .evidence
            .iter()
            .find(|s| s.identity == "imported_fn")
            .expect("imported_fn in evidence");
        assert!(!imp.defined);
        assert_eq!(imp.binding, "global");
    }

    #[test]
    fn private_dwarf_layout_is_uncertain_even_when_export_name_contains_type_name() {
        let Some((_left_dir, left)) = compile_so(
            "struct State { int a; }; static struct State state; int get_state_count(void) { state.a = 7; return state.a; }"
        ) else { return };
        let Some((_right_dir, right)) = compile_so(
            "struct State { int a; int b; }; static struct State state; int get_state_count(void) { state.a = 7; return state.a; }"
        ) else { return };
        let before = inspect_elf(&left, &std::fs::read(&left).unwrap());
        let after = inspect_elf(&right, &std::fs::read(&right).unwrap());
        assert!(before.abi.types.iter().any(|t| t == "State (size=4)"));
        assert!(after.abi.types.iter().any(|t| t == "State (size=8)"));
        let diff = crate::diff_reports(&before, &after);
        assert_eq!(diff.compatibility, lens_core::Compatibility::Uncertain);
        assert!(diff.symbols.added.is_empty() && diff.symbols.removed.is_empty());
    }

    #[test]
    fn test_demangle_symbol() {
        // C++ mangled symbol
        let cpp_mangled = "_ZNSt6vectorIiSaIiEE9push_backERKi";
        let demangled = demangle_symbol(cpp_mangled);
        assert!(demangled.is_some());
        let s = demangled.unwrap();
        assert!(s.contains("std::vector") && s.contains("push_back"));

        // Plain C symbol
        assert_eq!(demangle_symbol("simple_function"), None);
    }
}

#[cfg(test)]
mod dyn_tests {
    use object::Object;

    #[test]
    fn test_dynamic_metadata_parsing() {
        // The test binary itself is a dynamically linked ELF with PT_INTERP
        // and glibc version requirements on this platform.
        let exe = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let info = super::parse_dynamic(&file, &bytes);
        if file.section_by_name(".interp").is_some() {
            assert!(info.interpreter.is_some());
        }
        if file.section_by_name(".gnu.version_r").is_some() {
            assert!(!info.versions.is_empty());
            assert!(info.versions.iter().all(|v| !v.version.is_empty()));
        }
    }
}
