use crate::model::{ImpactReport, ParsedUnit};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

/// Upper bound on files scanned while resolving transitive includes for a
/// single translation unit — guards against include cycles blowing up the
/// traversal on pathological trees.
const MAX_SCANNED_FILES: usize = 4096;

#[derive(Debug, Clone, Default)]
pub struct ImpactGraph {
    /// header -> set of translation units directly depending on it
    pub header_to_units: BTreeMap<String, BTreeSet<String>>,
    /// header -> set of headers it includes
    pub header_to_headers: BTreeMap<String, BTreeSet<String>>,
    /// True when the on-disk include scan hit MAX_SCANNED_FILES — results
    /// may be incomplete.
    pub scan_truncated: bool,
    /// Translation units whose source file could not be read (e.g. deleted
    /// since the compile database was written).
    pub missing_sources: usize,
    /// `#include` directives that resolved to no on-disk file (generated
    /// headers, missing deps).
    pub unresolved_includes: usize,
    /// Per-file `#include` extraction cache — headers shared by many TUs
    /// are read and parsed once.
    include_cache: std::collections::HashMap<String, Option<Vec<IncludeDirective>>>,
}

impl ImpactGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_unit_include(&mut self, unit_file: &str, header_path: &str) {
        self.header_to_units
            .entry(header_path.to_string())
            .or_default()
            .insert(unit_file.to_string());
    }

    pub fn add_header_include(&mut self, parent_header: &str, included_header: &str) {
        self.header_to_headers
            .entry(parent_header.to_string())
            .or_default()
            .insert(included_header.to_string());
    }

    /// Scan `unit`'s source file on disk, resolve every `#include` against the
    /// unit's search paths, add direct unit->header edges, then recursively
    /// scan resolved headers to build the header->header transitive graph.
    pub fn add_translation_unit(&mut self, unit: &ParsedUnit) {
        let include_dirs: Vec<&str> = unit.includes.iter().map(|s| s.as_str()).collect();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(unit.file.clone());
        visited.insert(unit.file.clone());

        // -include headers behave as if the source included them first.
        // Per GCC they are searched from the preprocessor's working
        // directory (compile_commands `directory`), not the source's dir.
        let compile_dir = Path::new(&unit.directory);
        for inc in &unit.forced_includes {
            if let Some(resolved) = resolve_include(inc, true, compile_dir, &include_dirs) {
                self.add_unit_include(&unit.file, &resolved);
                if visited.insert(resolved.clone()) {
                    queue.push_back(resolved);
                }
            }
        }

        let mut scanned = 0usize;
        while let Some(current) = queue.pop_front() {
            if scanned >= MAX_SCANNED_FILES {
                self.scan_truncated = true;
                break;
            }
            // Cache include extraction per file: headers shared by many
            // translation units are read and parsed once per graph build.
            let includes = match self.include_cache.get(&current) {
                Some(cached) => cached.clone(),
                None => {
                    let parsed = lens_core::read_policy::read_to_string(&current)
                        .ok()
                        .map(|c| extract_includes(&c));
                    self.include_cache.insert(current.clone(), parsed.clone());
                    parsed
                }
            };
            let Some(includes) = includes else {
                if current == unit.file {
                    self.missing_sources += 1;
                }
                continue;
            };
            scanned += 1;
            let anchor = Path::new(&current)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            for inc in &includes {
                let Some(resolved) = resolve_include(&inc.name, inc.quoted, &anchor, &include_dirs)
                else {
                    self.unresolved_includes += 1;
                    continue;
                };
                if current == unit.file {
                    self.add_unit_include(&unit.file, &resolved);
                } else {
                    self.add_header_include(&current, &resolved);
                }
                if visited.insert(resolved.clone()) {
                    queue.push_back(resolved);
                }
            }
        }
    }

    /// Find all translation units impacted by changes to `target_header`.
    /// Transitive impact: if A.cpp includes B.h, and B.h includes C.h, modifying C.h impacts A.cpp.
    pub fn compute_impact(&self, target_header: &str) -> ImpactReport {
        // First, find all headers that directly or transitively include target_header.
        // We need the reverse of header_to_headers: included_header -> parent_headers
        let mut reverse_headers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (parent, includes) in &self.header_to_headers {
            for inc in includes {
                reverse_headers
                    .entry(inc.as_str())
                    .or_default()
                    .push(parent.as_str());
            }
        }

        let mut visited_headers = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(target_header);
        visited_headers.insert(target_header);

        let mut all_impacted_headers = Vec::new();
        while let Some(h) = queue.pop_front() {
            all_impacted_headers.push(h);
            if let Some(parents) = reverse_headers.get(h) {
                for &p in parents {
                    if visited_headers.insert(p) {
                        queue.push_back(p);
                    }
                }
            }
        }

        let mut impacted_units = BTreeSet::new();
        for h in all_impacted_headers {
            if let Some(units) = self.header_to_units.get(h) {
                for u in units {
                    impacted_units.insert(u.clone());
                }
            }
        }

        let impacted_vec: Vec<String> = impacted_units.into_iter().collect();
        let total = impacted_vec.len();

        ImpactReport {
            schema: crate::model::IMPACT_SCHEMA_V1.to_string(),
            target_header: target_header.to_string(),
            impacted_units: impacted_vec,
            total_impacted: total,
            scan_truncated: self.scan_truncated,
            missing_sources: self.missing_sources,
            unresolved_includes: self.unresolved_includes,
            hint: None,
        }
    }

    /// True when `h` names a header the graph knows: directly included by
    /// a translation unit, an includee of another header, or an includer.
    pub fn header_in_graph(&self, h: &str) -> bool {
        self.header_to_units.contains_key(h)
            || self.header_to_headers.contains_key(h)
            || self.header_to_headers.values().any(|s| s.contains(h))
    }

    /// Known headers whose basename matches `header`, for "did you mean"
    /// hints when a requested header is absent from the graph.
    pub fn closest_headers(&self, header: &str, max: usize) -> Vec<String> {
        let base = Path::new(header)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| header.to_string());
        let mut candidates: Vec<String> = self
            .header_to_units
            .keys()
            .chain(self.header_to_headers.keys())
            .chain(self.header_to_headers.values().flatten())
            .filter(|h| {
                Path::new(h.as_str())
                    .file_name()
                    .map(|f| f.to_string_lossy() == base)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        candidates.sort();
        candidates.dedup();
        candidates.truncate(max);
        candidates
    }
}

/// Resolve a possibly-relative header argument against `bases` in order
/// (compile-database directory, each entry's `directory`, caller's cwd):
/// the first base whose normalization lands in the graph wins. Absolute
/// headers normalize directly. When nothing resolves, fall back to the
/// first base so the report still names a concrete target.
pub fn resolve_header_target(graph: &ImpactGraph, header: &str, bases: &[String]) -> String {
    if Path::new(header).is_absolute() {
        return crate::normalize_path(header, "/");
    }
    bases
        .iter()
        .map(|b| crate::normalize_path(header, b))
        .find(|c| graph.header_in_graph(c))
        .unwrap_or_else(|| crate::normalize_path(header, bases.first().map_or(".", |b| b)))
}

/// An `#include` directive with its spelling preserved: `quoted` is true for
/// `"..."` form, false for `<...>` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeDirective {
    pub name: String,
    pub quoted: bool,
}

pub fn extract_includes(source_code: &str) -> Vec<IncludeDirective> {
    let mut includes = Vec::new();
    let mut in_block_comment = false;

    for line in source_code.lines() {
        let line = strip_comments(line, &mut in_block_comment);
        let trimmed = line.trim();
        let Some(after_hash) = trimmed.strip_prefix('#') else {
            continue;
        };
        let after_hash = after_hash.trim();
        let Some(rest) = after_hash
            .strip_prefix("include_next")
            .or_else(|| after_hash.strip_prefix("include"))
        else {
            continue;
        };
        let rest = rest.trim();
        // Take the first "..." or <...> span — anything after it (trailing
        // comments were already stripped) is ignored.
        let (quoted, inner) = if let Some(start) = rest.find('"') {
            match rest[start + 1..].find('"') {
                Some(end) => (true, &rest[start + 1..start + 1 + end]),
                None => continue,
            }
        } else if let Some(start) = rest.find('<') {
            match rest[start + 1..].find('>') {
                Some(end) => (false, &rest[start + 1..start + 1 + end]),
                None => continue,
            }
        } else {
            continue;
        };
        let inner = inner.trim();
        if !inner.is_empty() {
            includes.push(IncludeDirective {
                name: inner.to_string(),
                quoted,
            });
        }
    }

    includes
}

/// Remove `//` and `/* ... */` comments from one line, tracking block-comment
/// state across lines. String and char literals are respected so a `"//"` in
/// code is not mistaken for a comment.
fn strip_comments(line: &str, in_block: &mut bool) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if *in_block {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                *in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                out.push(quote as char);
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        out.push(bytes[i] as char);
                        out.push(bytes[i + 1] as char);
                        i += 2;
                    } else {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => break,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                *in_block = true;
                i += 2;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// Resolve an include name to an existing on-disk path. The returned path is
/// normalized (`./`/`..` collapsed) so the same header reached via different
/// spellings shares a single graph key.
///
/// Quoted includes search `anchor_dir` first (per the C/C++ standard that is
/// the including file's directory; for `-include` it is the compiler's
/// working directory), then the `-I`/`-isystem` search dirs; angle-bracket
/// includes only search the search dirs. Returns `None` when the header
/// cannot be found — system headers outside the project are expected to
/// miss.
fn resolve_include(
    name: &str,
    quoted: bool,
    anchor_dir: &Path,
    include_dirs: &[&str],
) -> Option<String> {
    if quoted {
        let candidate = anchor_dir.join(name);
        if candidate.is_file() {
            return Some(crate::compiler::clean_path(&candidate));
        }
    }
    for dir in include_dirs {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Some(crate::compiler::clean_path(&candidate));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ParsedUnit;

    fn tmp_root(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("lensbuild-impact-{tag}-"))
            .tempdir()
            .unwrap()
    }

    fn unit(file: &Path, dir: &Path, includes: &[&str]) -> ParsedUnit {
        ParsedUnit {
            file: file.to_string_lossy().into_owned(),
            directory: dir.to_string_lossy().into_owned(),
            compiler: "cc".to_string(),
            includes: includes.iter().map(|s| s.to_string()).collect(),
            defines: vec![],
            output: None,
            standard: None,
            flags: vec![],
            forced_includes: vec![],
        }
    }

    #[test]
    fn test_missing_sources_and_unresolved_includes_counted() {
        let root = tmp_root("gaps");
        let root = root.path();
        let inc = root.join("include");
        let src = root.join("src");
        std::fs::create_dir_all(&inc).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(inc.join("ok.h"), "#define OK 1\n").unwrap();
        std::fs::write(
            src.join("a.c"),
            "#include \"ok.h\"\n#include \"generated.h\"\n",
        )
        .unwrap();

        let mut graph = ImpactGraph::new();
        // a.c: one resolved + one unresolved include.
        graph.add_translation_unit(&unit(&src.join("a.c"), root, &[inc.to_str().unwrap()]));
        // deleted.c no longer exists on disk -> missing source.
        graph.add_translation_unit(&unit(&src.join("deleted.c"), root, &[]));

        assert_eq!(graph.missing_sources, 1);
        assert_eq!(graph.unresolved_includes, 1);

        let report = graph.compute_impact(&inc.join("ok.h").to_string_lossy());
        assert_eq!(report.missing_sources, 1);
        assert_eq!(report.unresolved_includes, 1);
        assert_eq!(report.total_impacted, 1);
    }

    #[test]
    fn test_include_cache_reuses_parsed_headers() {
        let root = tmp_root("cache");
        let root = root.path();
        let inc = root.join("inc");
        std::fs::create_dir_all(&inc).unwrap();
        // Shared by both TUs; would be re-read per TU without the cache.
        std::fs::write(inc.join("shared.h"), "#define S 1\n").unwrap();
        std::fs::write(root.join("a.c"), "#include \"shared.h\"\n").unwrap();
        std::fs::write(root.join("b.c"), "#include \"shared.h\"\n").unwrap();

        let mut graph = ImpactGraph::new();
        let incdir = inc.to_str().unwrap();
        graph.add_translation_unit(&unit(&root.join("a.c"), root, &[incdir]));
        graph.add_translation_unit(&unit(&root.join("b.c"), root, &[incdir]));

        let header = inc.join("shared.h").to_string_lossy().into_owned();
        assert_eq!(graph.header_to_units[&header].len(), 2);
        // One cache entry per distinct file (a.c, b.c, shared.h).
        assert_eq!(graph.include_cache.len(), 3);
    }
}
