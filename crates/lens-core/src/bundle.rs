//! Forensic flight recorder bundles (`.lens` files).
//!
//! A bundle is a gzip-compressed tar archive containing a `manifest.json`
//! (SHA-256 + size for every artifact) plus one JSON entry per collected
//! artifact under `reports/`. Create and verify share this module so the
//! two halves of the format can never drift apart.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::{Archive, Builder, Header};

use crate::error::{LensError, Result};
use crate::evidence::Source;
use crate::hash::digest_bytes;

pub const BUNDLE_SCHEMA_V2: &str = "lens.bundle/v2";
pub const MANIFEST_ENTRY: &str = "manifest.json";

/// Defensive caps against hostile archives (decompression bombs).
const MAX_BUNDLE_ENTRIES: usize = 4096;
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
/// Manifests are metadata, not payloads. Reject compressed diagnostic bombs
/// before reading or parsing the entry (including inspect/show/extract).
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The integrity inventory of a `.lens` bundle. It is unsigned: verification
/// detects corruption, not an attacker who can rewrite the whole archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema: String,
    pub tool: String,
    pub version: String,
    /// Real UTC creation time (RFC 3339). Never fabricated.
    pub created_at: String,
    #[serde(default)]
    pub sources: Vec<Source>,
    /// Collection failures recorded at bundle time — evidence of absence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    /// True when artifact bytes were scrubbed by the redactor before hashing.
    /// Older manifests lack the field and deserialize as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

/// One artifact destined for the bundle (typically a serialized report).
#[derive(Debug, Clone)]
pub struct BundleArtifact {
    /// Tar entry name, e.g. `reports/disk_snapshot.json`.
    pub name: String,
    pub data: Vec<u8>,
}

impl BundleArtifact {
    pub fn json<T: Serialize>(name: &str, value: &T) -> Result<Self> {
        Ok(Self {
            name: name.to_string(),
            data: crate::json::to_deterministic_pretty(value)?.into_bytes(),
        })
    }
}

/// True when `name` is a safe relative tar entry name (no absolute paths,
/// no `..` components, no backslashes or NUL).
fn valid_entry_name(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with("./")
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    !name.split('/').any(|c| c == ".." || c.is_empty())
}

/// Estimated uncompressed tar size: one 512-byte header per entry plus
/// the data padded up to a 512 boundary, plus the two zero end blocks.
fn estimated_archive_bytes(data_sizes: impl Iterator<Item = u64>) -> u64 {
    let mut total = 1024u64;
    for size in data_sizes {
        total += 512 + size.div_ceil(512) * 512;
    }
    total
}

/// Writes `artifacts` plus a generated `manifest.json` to `bundle_path` as a
/// `.tar.gz` archive and returns the manifest that was embedded. The archive
/// is staged to a temporary sibling file and atomically persisted on success,
/// so a failed write never leaves a partial bundle behind. `overwrite`
/// controls whether an existing bundle at `bundle_path` may be replaced.
/// Enforces the same entry/byte caps `verify` uses: a bundle too large to
/// verify is refused rather than written.
pub fn create_bundle_archive<P: AsRef<Path>>(
    bundle_path: P,
    tool_version: &str,
    artifacts: &[BundleArtifact],
    diagnostics: Vec<String>,
    overwrite: bool,
) -> Result<BundleManifest> {
    create_bundle_archive_with_redaction(
        bundle_path,
        tool_version,
        artifacts,
        diagnostics,
        overwrite,
        false,
    )
}

/// Like [`create_bundle_archive`], additionally recording in the manifest
/// whether the artifact bytes were passed through the redactor — so a shared
/// bundle is never mistaken for an unredacted original capture.
pub fn create_bundle_archive_with_redaction<P: AsRef<Path>>(
    bundle_path: P,
    tool_version: &str,
    artifacts: &[BundleArtifact],
    diagnostics: Vec<String>,
    overwrite: bool,
    redacted: bool,
) -> Result<BundleManifest> {
    let path_ref = bundle_path.as_ref();

    let mut seen = std::collections::HashSet::new();
    for art in artifacts {
        if !valid_entry_name(&art.name) {
            return Err(LensError::InvalidInput {
                message: format!("unsafe bundle entry name: {:?}", art.name),
            });
        }
        if !seen.insert(art.name.as_str()) {
            return Err(LensError::InvalidInput {
                message: format!("duplicate bundle entry name: {}", art.name),
            });
        }
        if art.name == MANIFEST_ENTRY {
            return Err(LensError::InvalidInput {
                message: format!("artifact name {} is reserved", MANIFEST_ENTRY),
            });
        }
    }

    // The archive must stay inside the caps verification enforces.
    if artifacts.len() + 1 > MAX_BUNDLE_ENTRIES {
        return Err(LensError::LimitExceeded {
            message: format!(
                "bundle would contain {} entries, exceeding the verify limit of {}",
                artifacts.len() + 1,
                MAX_BUNDLE_ENTRIES
            ),
        });
    }

    let mut sources = Vec::with_capacity(artifacts.len());
    for art in artifacts {
        sources.push(Source {
            path: art.name.clone(),
            sha256: digest_bytes(&art.data),
            size: art.data.len() as u64,
            ..Default::default()
        });
    }

    let manifest = BundleManifest {
        schema: BUNDLE_SCHEMA_V2.to_string(),
        tool: "lens".to_string(),
        version: tool_version.to_string(),
        created_at: crate::time::utc_now_iso(),
        sources,
        diagnostics,
        redacted,
    };
    // Gate before deterministic serialization clones strings into a Value.
    let _bounded = crate::json::to_bounded_vec(&manifest, MAX_MANIFEST_BYTES as usize)?;
    let manifest_json = crate::json::to_deterministic_pretty(&manifest)?.into_bytes();
    if manifest_json.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(LensError::LimitExceeded {
            message: "manifest exceeds maximum metadata size".into(),
        });
    }

    let estimated = estimated_archive_bytes(
        artifacts
            .iter()
            .map(|a| a.data.len() as u64)
            .chain(std::iter::once(manifest_json.len() as u64)),
    );
    if estimated > MAX_BUNDLE_BYTES {
        return Err(LensError::LimitExceeded {
            message: format!(
                "bundle (~{} estimated bytes) would exceed the verify limit of {} bytes",
                estimated, MAX_BUNDLE_BYTES
            ),
        });
    }

    let parent = path_ref
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| io_err(path_ref, e))?;

    let write_result = (|| -> std::result::Result<(), std::io::Error> {
        let gz = GzEncoder::new(tmp.as_file(), Compression::default());
        let mut tar = Builder::new(gz);
        let mut append = |name: &str, data: &[u8]| -> std::result::Result<(), std::io::Error> {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, data)
        };
        for art in artifacts {
            append(&art.name, &art.data)?;
        }
        append(MANIFEST_ENTRY, &manifest_json)?;
        let gz = tar.into_inner()?;
        gz.finish()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        // The NamedTempFile is deleted on drop — no partial bundle.
        return Err(io_err(path_ref, e));
    }

    let persisted = if overwrite {
        tmp.persist(path_ref).map(|_| ())
    } else {
        tmp.persist_noclobber(path_ref).map(|_| ())
    };
    match persisted {
        Ok(()) => Ok(manifest),
        Err(e) => {
            let e = e.error;
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Err(LensError::Usage {
                    message: format!(
                        "output bundle {:?} already exists; pass --force to overwrite",
                        path_ref
                    ),
                })
            } else {
                Err(io_err(path_ref, e))
            }
        }
    }
}

/// Verification report for a forensic bundle archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleVerificationReport {
    pub bundle_path: String,
    pub manifest_found: bool,
    /// Number of regular-file entries actually present in the archive.
    pub total_files: usize,
    pub verified_files: usize,
    pub tampered_files: Vec<String>,
    pub missing_files: Vec<String>,
    /// Archive entries not listed in the manifest (or listed twice).
    #[serde(default)]
    pub unexpected_files: Vec<String>,
    pub is_valid: bool,
    pub error: Option<String>,
}

/// Inspection summary used by `lens bundle inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleInspection {
    pub bundle_path: String,
    pub manifest: Option<BundleManifest>,
    pub entries: Vec<BundleEntryInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntryInfo {
    pub name: String,
    pub size: u64,
}

fn io_err(path: &Path, e: std::io::Error) -> LensError {
    LensError::Io {
        path: path.to_path_buf(),
        source: e,
    }
}

/// Read wrapper that turns a decompression bomb into a hard error once the
/// decoded stream exceeds `limit` bytes.
struct LimitedReader<R> {
    inner: R,
    remaining: u64,
    exceeded: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<R: std::io::Read> std::io::Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            self.exceeded
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bundle exceeds maximum decompressed size",
            ));
        }
        let max = self.remaining.min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Streams every entry of the archive, invoking `on_entry(name, reader)` for
/// regular files. Enforces an entry-count cap on *all* entry types and a cap
/// on actual decompressed bytes (not just header-declared sizes).
fn stream_entries<P, F>(bundle_path: P, mut on_entry: F) -> Result<()>
where
    P: AsRef<Path>,
    F: FnMut(&str, &mut dyn std::io::Read, u64) -> Result<()>,
{
    let path_ref = bundle_path.as_ref();
    let file = crate::read_policy::open(path_ref).map_err(|e| io_err(path_ref, e))?;
    let exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gz = LimitedReader {
        inner: GzDecoder::new(file),
        remaining: MAX_BUNDLE_BYTES,
        exceeded: exceeded.clone(),
    };
    let mut archive = Archive::new(gz);
    let entries = archive.entries().map_err(|e| io_err(path_ref, e))?;

    let map_io = |e: std::io::Error| -> LensError {
        if exceeded.load(std::sync::atomic::Ordering::SeqCst) {
            return LensError::LimitExceeded {
                message: format!("bundle exceeds {} decompressed bytes", MAX_BUNDLE_BYTES),
            };
        }
        io_err(path_ref, e)
    };

    let mut count = 0usize;
    for entry_res in entries {
        let mut entry = entry_res.map_err(map_io)?;
        count += 1;
        if count > MAX_BUNDLE_ENTRIES {
            return Err(LensError::LimitExceeded {
                message: format!("bundle exceeds {} entries", MAX_BUNDLE_ENTRIES),
            });
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(LensError::InvalidInput {
                message: format!(
                    "bundle contains unsupported non-regular tar entry type: {:?}",
                    entry_type
                ),
            });
        }
        let size = entry.header().size().unwrap_or(0);
        let name = entry.path().map_err(map_io)?.to_string_lossy().to_string();
        if normalize_entry_name(&name) == MANIFEST_ENTRY && size > MAX_MANIFEST_BYTES {
            return Err(LensError::LimitExceeded {
                message: "manifest exceeds maximum metadata size".into(),
            });
        }
        if let Err(err) = on_entry(&name, &mut entry, size) {
            if exceeded.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(LensError::LimitExceeded {
                    message: format!("bundle exceeds {} decompressed bytes", MAX_BUNDLE_BYTES),
                });
            }
            return Err(err);
        }
    }
    Ok(())
}

/// Normalizes an archive entry name for comparison: strips a leading `./`.
pub fn normalize_entry_name(name: &str) -> &str {
    name.strip_prefix("./").unwrap_or(name)
}

/// Compare collected entry hashes and sizes against the manifest — the validation
/// half shared by `verify_bundle_archive` (hashes computed while
/// streaming) and `extract_bundle` (hashes computed while staging).
fn evaluate_bundle_contents(
    path_ref: &Path,
    file_hashes: HashMap<String, (String, u64)>,
    manifest_content: Option<Vec<u8>>,
    manifest_count: usize,
) -> BundleVerificationReport {
    let invalid = |report: &mut BundleVerificationReport, msg: String| {
        report.error = Some(msg);
        report.is_valid = false;
    };

    let mut report = BundleVerificationReport {
        bundle_path: path_ref.display().to_string(),
        manifest_found: false,
        total_files: file_hashes.len(),
        verified_files: 0,
        tampered_files: Vec::new(),
        missing_files: Vec::new(),
        unexpected_files: Vec::new(),
        is_valid: false,
        error: None,
    };

    if manifest_count > 1 {
        invalid(
            &mut report,
            format!("Archive contains {} manifest.json entries", manifest_count),
        );
        return report;
    }

    let manifest_bytes = match manifest_content {
        Some(b) => b,
        None => {
            invalid(
                &mut report,
                "Archive does not contain manifest.json".to_string(),
            );
            return report;
        }
    };
    report.manifest_found = true;

    let manifest: BundleManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(m) => m,
        Err(e) => {
            invalid(&mut report, format!("Invalid manifest.json: {}", e));
            return report;
        }
    };

    if manifest.schema != BUNDLE_SCHEMA_V2 {
        invalid(
            &mut report,
            format!(
                "Unsupported manifest schema {:?} (expected {:?})",
                manifest.schema, BUNDLE_SCHEMA_V2
            ),
        );
        return report;
    }

    let mut consumed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for src in &manifest.sources {
        let normalized = normalize_entry_name(&src.path);
        if !consumed.insert(normalized) {
            report
                .unexpected_files
                .push(format!("duplicate manifest source: {}", normalized));
            continue;
        }
        match file_hashes.get(normalized) {
            Some((actual_hash, actual_size)) => {
                if actual_hash == "DUPLICATE" {
                    report
                        .unexpected_files
                        .push(format!("duplicate entry: {}", normalized));
                } else if !actual_hash.eq_ignore_ascii_case(&src.sha256) {
                    report.tampered_files.push(format!(
                        "{}: expected {} got {}",
                        src.path, src.sha256, actual_hash
                    ));
                } else if *actual_size != src.size {
                    report.tampered_files.push(format!(
                        "{}: expected size {} got {}",
                        src.path, src.size, actual_size
                    ));
                } else {
                    report.verified_files += 1;
                }
            }
            None => report.missing_files.push(src.path.clone()),
        }
    }
    for key in file_hashes.keys() {
        if !consumed.contains(key.as_str()) {
            report.unexpected_files.push(key.clone());
        }
    }
    report.unexpected_files.sort();

    report.is_valid = report.tampered_files.is_empty()
        && report.missing_files.is_empty()
        && report.unexpected_files.is_empty()
        && !manifest.sources.is_empty();
    report
}

/// Verifies every manifest-listed file's SHA-256 and byte size against the archive contents.
pub fn verify_bundle_archive<P: AsRef<Path>>(bundle_path: P) -> Result<BundleVerificationReport> {
    let path_ref = bundle_path.as_ref();

    let mut file_hashes: HashMap<String, (String, u64)> = HashMap::new();
    let mut manifest_content: Option<Vec<u8>> = None;
    let mut manifest_count = 0usize;

    stream_entries(path_ref, |name, reader, _size| {
        // Only an exact top-level `manifest.json` (optionally `./`-prefixed)
        // is the manifest. `foo/manifest.json` is an ordinary artifact.
        if normalize_entry_name(name) == MANIFEST_ENTRY {
            manifest_count += 1;
            let mut content = Vec::new();
            reader
                .take(MAX_BUNDLE_BYTES)
                .read_to_end(&mut content)
                .map_err(|e| io_err(path_ref, e))?;
            manifest_content = Some(content);
        } else {
            let mut hasher = crate::hash::IncrementalHasher::new();
            let mut actual_size = 0u64;
            let mut buf = [0u8; 65536];
            loop {
                let n = reader.read(&mut buf).map_err(|e| io_err(path_ref, e))?;
                if n == 0 {
                    break;
                }
                actual_size += n as u64;
                hasher.update(&buf[..n]);
            }
            let hash = hasher.finish();
            let key = normalize_entry_name(name).to_string();
            if file_hashes
                .insert(key.clone(), (hash, actual_size))
                .is_some()
            {
                // Duplicate entry name — archive is not reproducible evidence.
                file_hashes.insert(key, ("DUPLICATE".to_string(), actual_size));
            }
        }
        Ok(())
    })?;

    Ok(evaluate_bundle_contents(
        path_ref,
        file_hashes,
        manifest_content,
        manifest_count,
    ))
}

/// Reads the manifest and entry listing of a bundle without hashing payloads.
pub fn inspect_bundle<P: AsRef<Path>>(bundle_path: P) -> Result<BundleInspection> {
    let path_ref = bundle_path.as_ref();
    let mut manifest_bytes = None;
    let mut entries = Vec::new();

    stream_entries(path_ref, |name, reader, size| {
        entries.push(BundleEntryInfo {
            name: name.to_string(),
            size,
        });
        if normalize_entry_name(name) == MANIFEST_ENTRY && manifest_bytes.is_none() {
            let mut content = Vec::new();
            reader
                .take(MAX_BUNDLE_BYTES)
                .read_to_end(&mut content)
                .map_err(|e| io_err(path_ref, e))?;
            manifest_bytes = Some(content);
        }
        Ok(())
    })?;

    let manifest = match manifest_bytes {
        Some(b) => Some(serde_json::from_slice::<BundleManifest>(&b).map_err(|e| {
            LensError::InvalidInput {
                message: format!("Invalid manifest.json: {}", e),
            }
        })?),
        None => None,
    };

    Ok(BundleInspection {
        bundle_path: path_ref.display().to_string(),
        manifest,
        entries,
    })
}

/// Read a single entry's bytes by exact (normalized) name. Enforces the
/// same traversal, size, duplicate, and SHA-256 integrity rules as verification.
pub fn read_bundle_entry<P: AsRef<Path>>(bundle_path: P, name: &str) -> Result<Vec<u8>> {
    if !valid_entry_name(name) {
        return Err(LensError::InvalidInput {
            message: format!("unsafe or empty bundle entry name: {:?}", name),
        });
    }
    let path_ref = bundle_path.as_ref();
    let want = normalize_entry_name(name);
    let mut found: Option<Vec<u8>> = None;
    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut manifest_count = 0usize;

    stream_entries(path_ref, |entry_name, reader, _size| {
        let norm = normalize_entry_name(entry_name);
        if norm == MANIFEST_ENTRY {
            manifest_count += 1;
            let mut buf = Vec::new();
            reader
                .take(MAX_BUNDLE_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| io_err(path_ref, e))?;
            if want == MANIFEST_ENTRY {
                if found.is_some() {
                    return Err(LensError::InvalidInput {
                        message: "duplicate manifest.json in bundle".to_string(),
                    });
                }
                found = Some(buf.clone());
            }
            manifest_bytes = Some(buf);
        } else if norm == want {
            if found.is_some() {
                return Err(LensError::InvalidInput {
                    message: format!("duplicate entry {:?} in bundle", name),
                });
            }
            let mut buf = Vec::new();
            reader
                .take(MAX_BUNDLE_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| io_err(path_ref, e))?;
            found = Some(buf);
        }
        Ok(())
    })?;

    if manifest_count > 1 {
        return Err(LensError::InvalidInput {
            message: format!("bundle contains {} manifest.json entries", manifest_count),
        });
    }

    let bytes = found.ok_or_else(|| LensError::InvalidInput {
        message: format!("entry {:?} not found in bundle", name),
    })?;

    if want != MANIFEST_ENTRY {
        let m_bytes = manifest_bytes.ok_or_else(|| LensError::InvalidInput {
            message: "bundle does not contain manifest.json".to_string(),
        })?;
        let manifest: BundleManifest =
            serde_json::from_slice(&m_bytes).map_err(|e| LensError::InvalidInput {
                message: format!("Invalid manifest.json: {}", e),
            })?;
        if manifest.schema != BUNDLE_SCHEMA_V2 {
            return Err(LensError::InvalidInput {
                message: format!(
                    "Unsupported manifest schema {:?} (expected {:?})",
                    manifest.schema, BUNDLE_SCHEMA_V2
                ),
            });
        }
        let mut matching = manifest
            .sources
            .iter()
            .filter(|s| normalize_entry_name(&s.path) == want);
        let src = matching.next().ok_or_else(|| LensError::InvalidInput {
            message: format!("entry {:?} is not listed in bundle manifest", name),
        })?;
        if matching.next().is_some() {
            return Err(LensError::InvalidInput {
                message: format!("bundle manifest lists entry {:?} more than once", name),
            });
        }
        let actual_hash = digest_bytes(&bytes);
        if !actual_hash.eq_ignore_ascii_case(&src.sha256) || bytes.len() as u64 != src.size {
            return Err(LensError::InvalidInput {
                message: format!("bundle entry {:?} failed integrity verification", name),
            });
        }
    }

    Ok(bytes)
}

/// Check (and when `create`, fill in) the ancestor chain of `name`
/// between `dest_dir` and the target: every existing component must be a
/// real directory — a symlink or non-directory refuses extraction, so a
/// prepared destination cannot redirect writes outside it.
fn check_ancestors(dest_dir: &Path, name: &str, create: bool) -> Result<()> {
    let Some(rel_parent) = Path::new(name).parent() else {
        return Ok(());
    };
    let mut prefix = dest_dir.to_path_buf();
    for comp in rel_parent.components() {
        prefix.push(comp.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(LensError::InvalidInput {
                    message: format!("refusing to extract through symlink {:?}", prefix),
                });
            }
            Ok(md) if md.is_dir() => {}
            Ok(_) => {
                return Err(LensError::InvalidInput {
                    message: format!("refusing to extract through non-directory {:?}", prefix),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    std::fs::create_dir(&prefix).map_err(|e| io_err(&prefix, e))?;
                }
            }
            Err(e) => return Err(io_err(&prefix, e)),
        }
    }
    Ok(())
}

/// Check whether `target` may be written: existing paths are errors
/// without `force`; symlinks and directories are errors even with it.
fn check_target(target: &Path, force: bool) -> Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(md) => {
            if !force {
                return Err(LensError::InvalidInput {
                    message: format!("{:?} already exists; pass --force to overwrite", target),
                });
            }
            // Even with force, never delete a symlink or a directory:
            // removing a symlink then writing through its name would
            // plant a file the attacker chose the location of.
            if md.file_type().is_symlink() || md.is_dir() {
                return Err(LensError::InvalidInput {
                    message: format!("refusing to overwrite {:?}: not a regular file", target),
                });
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(target, e)),
    }
}

/// Move verified staged entries into place under `dest_dir`. Placement is
/// all-or-nothing: the ancestor and target-conflict checks run for every
/// entry first — nothing is created or moved when any entry would fail —
/// and only then are missing directories created, regular-file targets
/// replaced (with `force`), and staged files renamed into place. Returns
/// the sorted list of written paths.
fn place_staged(
    staged: &[(String, PathBuf)],
    dest_dir: &Path,
    force: bool,
) -> Result<Vec<PathBuf>> {
    for (name, _) in staged {
        check_ancestors(dest_dir, name, false)?;
        check_target(&dest_dir.join(name), force)?;
    }

    let mut written = Vec::with_capacity(staged.len());
    for (name, stage_path) in staged {
        let target = dest_dir.join(name);
        check_ancestors(dest_dir, name, true)?;
        #[cfg(not(unix))]
        if force && target.symlink_metadata().is_ok() {
            // Pre-checked: only a regular file reaches this point.
            std::fs::remove_file(&target).map_err(|e| io_err(&target, e))?;
        }
        // On Unix, rename(2) atomically replaces an existing regular file.
        std::fs::rename(stage_path, &target).map_err(|e| io_err(&target, e))?;
        written.push(target);
    }
    written.sort();
    Ok(written)
}

/// Verify then extract a bundle into `dest_dir` in a single streaming
/// pass: entries are written to a staging directory inside `dest_dir`
/// (cleaned up on drop) while their SHA-256 is computed, and only if the
/// integrity check passes are they moved into place — a tampered bundle
/// never leaves extracted files behind. Entry names are re-validated,
/// symlinks in the destination are never followed, and existing files
/// are not overwritten unless `force` is set. Returns extracted paths.
pub fn extract_bundle<P: AsRef<Path>>(
    bundle_path: P,
    dest_dir: P,
    force: bool,
) -> Result<Vec<PathBuf>> {
    let bundle_path = bundle_path.as_ref();
    let dest_dir = dest_dir.as_ref();

    if let Ok(md) = std::fs::symlink_metadata(dest_dir) {
        if md.file_type().is_symlink() {
            return Err(LensError::InvalidInput {
                message: format!("refusing to extract into symlink {:?}", dest_dir),
            });
        }
    }

    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;
    let staging = tempfile::Builder::new()
        .prefix(".lens-extract-")
        .tempdir_in(dest_dir)
        .map_err(|e| io_err(dest_dir, e))?;

    let mut file_hashes: HashMap<String, (String, u64)> = HashMap::new();
    let mut manifest_content: Option<Vec<u8>> = None;
    let mut manifest_count = 0usize;
    let mut staged: Vec<(String, PathBuf)> = Vec::new();

    stream_entries(bundle_path, |name, reader, _size| {
        let name = normalize_entry_name(name);
        if !valid_entry_name(name) {
            return Err(LensError::InvalidInput {
                message: format!("unsafe bundle entry name: {:?}", name),
            });
        }
        // Belt-and-braces: join+normalize again, ensure it stays under dest.
        let target = dest_dir.join(name);
        let normalized_target = normalize_path_str(&target.to_string_lossy());
        let normalized_dest = normalize_path_str(&dest_dir.to_string_lossy());
        let stays_within_dest = if normalized_dest.is_empty() {
            !normalized_target.is_empty() && !normalized_target.starts_with('/')
        } else if normalized_dest == "/" {
            normalized_target.starts_with('/') && normalized_target.len() > 1
        } else {
            let prefix = format!("{}/", normalized_dest.trim_end_matches('/'));
            format!("{normalized_target}/").starts_with(&prefix)
        };
        if !stays_within_dest {
            return Err(LensError::InvalidInput {
                message: format!("entry {:?} escapes destination", name),
            });
        }
        let stage_path = staging.path().join(name);
        if let Some(parent) = stage_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        if name == MANIFEST_ENTRY {
            // Only an exact top-level `manifest.json` is the manifest.
            manifest_count += 1;
            let mut content = Vec::new();
            reader
                .take(MAX_BUNDLE_BYTES)
                .read_to_end(&mut content)
                .map_err(|e| io_err(bundle_path, e))?;
            std::fs::write(&stage_path, &content).map_err(|e| io_err(&stage_path, e))?;
            manifest_content = Some(content);
            staged.push((name.to_string(), stage_path));
            return Ok(());
        }
        // Stage the entry while hashing it — one pass over the bytes.
        let mut file = File::create(&stage_path).map_err(|e| io_err(&stage_path, e))?;
        let mut hasher = crate::hash::IncrementalHasher::new();
        let mut actual_size = 0u64;
        let mut limited = reader.take(MAX_BUNDLE_BYTES);
        let mut buf = [0u8; 65536];
        loop {
            let n = limited.read(&mut buf).map_err(|e| io_err(bundle_path, e))?;
            if n == 0 {
                break;
            }
            actual_size += n as u64;
            std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| io_err(&stage_path, e))?;
            hasher.update(&buf[..n]);
        }
        let hash = hasher.finish();
        let key = name.to_string();
        if file_hashes
            .insert(key.clone(), (hash, actual_size))
            .is_some()
        {
            // Duplicate entry name — archive is not reproducible evidence.
            file_hashes.insert(key, ("DUPLICATE".to_string(), actual_size));
        }
        staged.push((name.to_string(), stage_path));
        Ok(())
    })?;

    // Evidence integrity first: never extract a bundle that fails
    // verification. The staging dir is dropped (and removed) on error,
    // so nothing staged survives a failed check.
    let report =
        evaluate_bundle_contents(bundle_path, file_hashes, manifest_content, manifest_count);
    if !report.is_valid {
        return Err(LensError::InvalidInput {
            message: format!(
                "bundle integrity check failed: {}",
                report.error.unwrap_or_else(|| {
                    format!(
                        "{} tampered, {} missing, {} unexpected file(s)",
                        report.tampered_files.len(),
                        report.missing_files.len(),
                        report.unexpected_files.len()
                    )
                })
            ),
        });
    }

    place_staged(&staged, dest_dir, force)
}

/// Lexically normalize a path string (`.`/`..`/`//` collapsed).
fn normalize_path_str(s: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = parts.join("/");
    if s.starts_with('/') {
        out = format!("/{}", out);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_manifest_is_rejected_before_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.lens");
        create_bundle_archive(&path, "test", &sample_artifacts(), vec![], false).unwrap();
        rebuild_with(&path, |entries| {
            let manifest = entries
                .iter_mut()
                .find(|(name, _)| name == MANIFEST_ENTRY)
                .unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
            value["diagnostics"] = serde_json::json!(["x".repeat(8 * 1024 * 1024)]);
            manifest.1 = serde_json::to_vec(&value).unwrap();
        });
        assert!(std::fs::metadata(&path).unwrap().len() < 64 * 1024);
        assert!(matches!(
            verify_bundle_archive(&path),
            Err(LensError::LimitExceeded { .. })
        ));
        assert!(matches!(
            inspect_bundle(&path),
            Err(LensError::LimitExceeded { .. })
        ));
        assert!(matches!(
            read_bundle_entry(&path, MANIFEST_ENTRY),
            Err(LensError::LimitExceeded { .. })
        ));
        assert!(matches!(
            extract_bundle(&path, &dir.path().join("extract"), false),
            Err(LensError::LimitExceeded { .. })
        ));
        assert!(create_bundle_archive(
            dir.path().join("create.lens"),
            "test",
            &sample_artifacts(),
            vec!["x".repeat(2 * 1024 * 1024)],
            false
        )
        .is_err());
    }
    use super::*;
    use tempfile::NamedTempFile;

    fn sample_artifacts() -> Vec<BundleArtifact> {
        vec![
            BundleArtifact::json(
                "reports/disk_snapshot.json",
                &serde_json::json!({"schema": "diskmap.snapshot/v2", "node_count": 7}),
            )
            .unwrap(),
            BundleArtifact::json(
                "reports/net_report.json",
                &serde_json::json!({"schema_version": "lens.net/v1"}),
            )
            .unwrap(),
        ]
    }

    #[test]
    fn test_create_verify_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let manifest =
            create_bundle_archive(tmp.path(), "0.5.0-test", &sample_artifacts(), vec![], true)
                .unwrap();

        assert_eq!(manifest.schema, BUNDLE_SCHEMA_V2);
        assert_eq!(manifest.sources.len(), 2);
        assert!(manifest.created_at.ends_with('Z'));

        let report = verify_bundle_archive(tmp.path()).unwrap();
        assert!(report.is_valid, "report was: {:?}", report);
        assert_eq!(report.verified_files, 2);
        assert_eq!(report.total_files, 2);
        assert!(report.tampered_files.is_empty());
        assert!(report.missing_files.is_empty());

        let inspection = inspect_bundle(tmp.path()).unwrap();
        assert_eq!(inspection.entries.len(), 3); // 2 reports + manifest
        let m = inspection.manifest.unwrap();
        assert_eq!(m.tool, "lens");
        assert_eq!(m.version, "0.5.0-test");
    }

    fn rebuild_with(path: &Path, mutate: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) {
        let raw = std::fs::read(path).unwrap();
        let mut archive = Archive::new(GzDecoder::new(&raw[..]));
        let mut rebuilt: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in archive.entries().unwrap().flatten() {
            let mut e = entry;
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut data).unwrap();
            let name = e.path().unwrap().to_string_lossy().into_owned();
            rebuilt.push((name, data));
        }
        mutate(&mut rebuilt);
        let out = File::create(path).unwrap();
        let gz = GzEncoder::new(out, Compression::default());
        let mut builder = Builder::new(gz);
        for (name, data) in rebuilt {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, data.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn test_redacted_flag_recorded_and_omitted_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.lens");
        let scrubbed = dir.path().join("scrubbed.lens");
        let m_plain =
            create_bundle_archive(&plain, "x", &sample_artifacts(), vec![], false).unwrap();
        let m_red = create_bundle_archive_with_redaction(
            &scrubbed,
            "x",
            &sample_artifacts(),
            vec![],
            false,
            true,
        )
        .unwrap();
        assert!(!m_plain.redacted);
        assert!(m_red.redacted);

        // Default bundles keep the exact pre-existing manifest bytes: the
        // field is omitted when false, so older tooling/hashes are unaffected.
        let plain_manifest = read_bundle_entry(&plain, MANIFEST_ENTRY).unwrap();
        assert!(!String::from_utf8_lossy(&plain_manifest).contains("redacted"));

        let inspected = inspect_bundle(&scrubbed).unwrap();
        assert!(inspected.manifest.unwrap().redacted);
        assert!(verify_bundle_archive(&scrubbed).unwrap().is_valid);
    }

    #[test]
    fn test_show_rejects_duplicate_manifest_source() {
        let tmp = NamedTempFile::new().unwrap();
        create_bundle_archive(tmp.path(), "x", &sample_artifacts(), vec![], true).unwrap();
        rebuild_with(tmp.path(), |entries| {
            for (name, data) in entries.iter_mut() {
                if name == MANIFEST_ENTRY {
                    let mut m: serde_json::Value = serde_json::from_slice(data).unwrap();
                    let first = m["sources"][0].clone();
                    m["sources"].as_array_mut().unwrap().push(first);
                    *data = serde_json::to_vec(&m).unwrap();
                }
            }
        });
        assert!(!verify_bundle_archive(tmp.path()).unwrap().is_valid);
        let err = read_bundle_entry(tmp.path(), "reports/disk_snapshot.json").unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn test_verify_detects_tampering() {
        let tmp = NamedTempFile::new().unwrap();
        create_bundle_archive(tmp.path(), "x", &sample_artifacts(), vec![], true).unwrap();

        // Rebuild the archive deterministically with one artifact's payload
        // altered — bit-flipping the compressed stream is unreliable (the
        // flipped byte can land in tar padding that nothing hashes).
        rebuild_with(tmp.path(), |entries| {
            for (name, data) in entries.iter_mut() {
                if name == "reports/disk_snapshot.json" {
                    data[0] ^= 0xFF; // tamper the artifact, keep the manifest intact
                }
            }
        });

        let report = verify_bundle_archive(tmp.path()).unwrap();
        assert!(!report.is_valid);
        assert_eq!(report.tampered_files.len(), 1);
        assert!(report.tampered_files[0].contains("disk_snapshot"));
    }

    #[test]
    fn test_verify_rejects_spoofed_nested_manifest() {
        let tmp = NamedTempFile::new().unwrap();
        create_bundle_archive(tmp.path(), "x", &sample_artifacts(), vec![], true).unwrap();

        // Attack: tamper an artifact, then append an `evil/manifest.json`
        // claiming the tampered hash. An older suffix-match verifier would
        // treat the appended file as the manifest and verify "clean".
        rebuild_with(tmp.path(), |entries| {
            for (name, data) in entries.iter_mut() {
                if name == "reports/disk_snapshot.json" {
                    data[0] ^= 0xFF;
                }
            }
            let fake = serde_json::json!({
                "schema": BUNDLE_SCHEMA_V2,
                "tool": "lens",
                "version": "x",
                "created_at": "2026-01-01T00:00:00Z",
                "sources": []
            });
            entries.push((
                "evil/manifest.json".to_string(),
                serde_json::to_vec_pretty(&fake).unwrap(),
            ));
        });

        let report = verify_bundle_archive(tmp.path()).unwrap();
        assert!(!report.is_valid);
        // Tampering is still caught, and the injected manifest is flagged.
        assert_eq!(report.tampered_files.len(), 1);
        assert_eq!(report.unexpected_files, vec!["evil/manifest.json"]);
    }

    #[test]
    fn test_verify_rejects_unlisted_entries() {
        let tmp = NamedTempFile::new().unwrap();
        create_bundle_archive(tmp.path(), "x", &sample_artifacts(), vec![], true).unwrap();

        rebuild_with(tmp.path(), |entries| {
            entries.push((
                "reports/smuggled.json".to_string(),
                b"{\"hidden\": true}".to_vec(),
            ));
        });

        let report = verify_bundle_archive(tmp.path()).unwrap();
        assert!(!report.is_valid);
        assert_eq!(report.unexpected_files, vec!["reports/smuggled.json"]);
    }

    #[test]
    fn test_create_rejects_unsafe_entry_names() {
        let tmp = NamedTempFile::new().unwrap();
        for bad in ["../x.json", "/abs.json", "a//b.json", "manifest.json"] {
            let artifacts = vec![BundleArtifact {
                name: bad.to_string(),
                data: b"{}".to_vec(),
            }];
            assert!(
                create_bundle_archive(tmp.path(), "x", &artifacts, vec![], true).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn test_failed_create_leaves_no_partial_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.lens");
        let artifacts = vec![
            BundleArtifact {
                name: "ok.json".to_string(),
                data: b"{}".to_vec(),
            },
            BundleArtifact {
                name: "bad/../name".to_string(),
                data: b"{}".to_vec(),
            },
        ];
        assert!(create_bundle_archive(&out, "x", &artifacts, vec![], false).is_err());
        assert!(!out.exists());
        assert!(!dir.path().join("out.lens.tmp").exists());
    }

    #[test]
    fn test_read_bundle_entry() {
        let tmp = NamedTempFile::new().unwrap();
        create_bundle_archive(tmp.path(), "x", &sample_artifacts(), vec![], true).unwrap();

        let bytes = read_bundle_entry(tmp.path(), "reports/disk_snapshot.json").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["schema"], "diskmap.snapshot/v2");

        // Missing and unsafe names are rejected.
        assert!(read_bundle_entry(tmp.path(), "reports/nope.json").is_err());
        assert!(read_bundle_entry(tmp.path(), "../evil").is_err());
        assert!(read_bundle_entry(tmp.path(), "/abs").is_err());
    }

    #[test]
    fn test_extract_verifies_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        let dest = dir.path().join("out");
        let written = extract_bundle(&bundle, &dest, false).unwrap();
        assert_eq!(written.len(), 3); // 2 artifacts + manifest
        assert!(dest.join("reports/disk_snapshot.json").exists());
        assert!(dest.join("manifest.json").exists());

        // Refuse overwrite without force; succeed with it.
        assert!(extract_bundle(&bundle, &dest, false).is_err());
        assert!(extract_bundle(&bundle, &dest, true).is_ok());
    }

    #[test]
    fn test_extract_refuses_tampered_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();
        rebuild_with(&bundle, |entries| {
            for (name, data) in entries.iter_mut() {
                if name == "reports/disk_snapshot.json" {
                    data[0] ^= 0xFF;
                }
            }
        });
        let dest = dir.path().join("out");
        assert!(extract_bundle(&bundle, &dest, false).is_err());
        assert!(!dest.join("reports/disk_snapshot.json").exists());
    }

    /// Minimal tar writer for hostile fixtures — the `tar` crate refuses
    /// `..` names, which is exactly what this test needs to produce.
    fn raw_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            let mut hdr = [0u8; 512];
            hdr[..name.len()].copy_from_slice(name.as_bytes());
            hdr[100..108].copy_from_slice(b"0000644\0");
            hdr[108..116].copy_from_slice(b"0000000\0");
            hdr[116..124].copy_from_slice(b"0000000\0");
            hdr[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            hdr[136..148].copy_from_slice(b"00000000000\0");
            hdr[148..156].copy_from_slice(b"        ");
            hdr[156] = b'0';
            let cksum: u64 = hdr.iter().map(|&b| b as u64).sum();
            hdr[148..156].copy_from_slice(format!("{:06o}\0 ", cksum).as_bytes());
            out.extend_from_slice(&hdr);
            out.extend_from_slice(data);
            let pad = (512 - data.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0, pad));
        }
        out.extend(std::iter::repeat_n(0, 1024));
        out
    }

    #[test]
    fn test_extract_rejects_traversal_entries() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("evil.lens");
        // The manifest even lists the traversal entry with a correct hash
        // (verify passes) — the extractor's own name check must refuse it.
        let payload: &[u8] = b"owned";
        let manifest = serde_json::json!({
            "schema": BUNDLE_SCHEMA_V2,
            "tool": "lens",
            "version": "x",
            "created_at": "2026-01-01T00:00:00Z",
            "sources": [{
                "path": "../escape.txt",
                "sha256": digest_bytes(payload),
                "size": payload.len(),
            }]
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let tar = raw_tar(&[
            ("../escape.txt", payload),
            ("manifest.json", &manifest_bytes),
        ]);
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        std::fs::write(&bundle, enc.finish().unwrap()).unwrap();

        let dest = dir.path().join("out");
        assert!(extract_bundle(&bundle, &dest, true).is_err());
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[test]
    fn test_create_no_overwrite_refuses_existing_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("b.lens");
        create_bundle_archive(&out, "x", &sample_artifacts(), vec![], false).unwrap();
        let original = std::fs::read(&out).unwrap();

        // Without overwrite, create refuses and the original is intact.
        let err = create_bundle_archive(&out, "x", &sample_artifacts(), vec![], false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), original);

        // With overwrite it succeeds.
        create_bundle_archive(&out, "x", &sample_artifacts(), vec![], true).unwrap();
    }

    #[test]
    fn test_create_refuses_bundles_over_verify_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("big.lens");
        let artifacts: Vec<BundleArtifact> = (0..MAX_BUNDLE_ENTRIES)
            .map(|i| BundleArtifact {
                name: format!("reports/f{i}.json"),
                data: b"{}".to_vec(),
            })
            .collect();
        let err = create_bundle_archive(&out, "x", &artifacts, vec![], false).unwrap_err();
        assert!(
            matches!(err, LensError::LimitExceeded { .. }),
            "expected LimitExceeded, got {err:?}"
        );
        assert!(!out.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_refuses_symlinked_ancestor_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        // dest/reports is a symlink out of the destination: extraction
        // must refuse rather than write through it.
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let dest = dir.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("reports")).unwrap();

        for force in [false, true] {
            let err = extract_bundle(&bundle, &dest, force).unwrap_err();
            assert!(err.to_string().contains("symlink"), "{err}");
            // Nothing was written outside, and the symlink is untouched.
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
            assert!(dest
                .join("reports")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_refuses_to_overwrite_symlink_target_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        // An attacker-planted symlink where an entry would land: force
        // must not remove it or write through it.
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "keep me").unwrap();
        let dest = dir.path().join("out");
        let reports = dest.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        std::os::unix::fs::symlink(&victim, reports.join("disk_snapshot.json")).unwrap();

        let err = extract_bundle(&bundle, &dest, true).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        assert!(reports
            .join("disk_snapshot.json")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn test_extract_conflict_aborts_before_placing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        // dest already holds reports/net_report.json but not the other
        // entry: the conflict must abort the whole placement — the
        // earlier-staged disk_snapshot.json is not written either.
        let dest = dir.path().join("out");
        let reports = dest.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::write(reports.join("net_report.json"), "keep").unwrap();

        let err = extract_bundle(&bundle, &dest, false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(!reports.join("disk_snapshot.json").exists());
        assert!(!dest.join("manifest.json").exists());
        assert_eq!(
            std::fs::read_to_string(reports.join("net_report.json")).unwrap(),
            "keep"
        );
        // And the staging dir was cleaned up.
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
    }

    #[test]
    fn test_extract_tampered_bundle_leaves_dest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();
        rebuild_with(&bundle, |entries| {
            for (name, data) in entries.iter_mut() {
                if name == "reports/disk_snapshot.json" {
                    data[0] ^= 0xFF;
                }
            }
        });

        let dest = dir.path().join("out");
        assert!(extract_bundle(&bundle, &dest, false).is_err());
        // Staging happened inside dest and was cleaned on drop: no
        // extracted files and no leftover `.lens-extract-*` dirs.
        assert!(dest.exists());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    }

    #[test]
    fn test_extract_into_dot_relative_path_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        let sub = dir.path().join("work");
        std::fs::create_dir_all(&sub).unwrap();
        // Verify "." normalization logic directly via relative path with "./"
        let rel_dot = sub.join(".");
        let written = extract_bundle(&bundle, &rel_dot, false).unwrap();
        assert_eq!(written.len(), 3);
        assert!(sub.join("manifest.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_refuses_symlink_dest_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();

        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let symlink_dest = dir.path().join("sym_dest");
        std::os::unix::fs::symlink(&outside, &symlink_dest).unwrap();

        let err = extract_bundle(&bundle, &symlink_dest, true).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn test_corrupt_gzip_is_not_misclassified_as_limit_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt = dir.path().join("corrupt.lens");
        std::fs::write(&corrupt, b"not a valid gzip stream at all").unwrap();

        let err = verify_bundle_archive(&corrupt).unwrap_err();
        assert!(
            !matches!(err, LensError::LimitExceeded { .. }),
            "corrupt gzip must not report LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn test_read_bundle_entry_rejects_tampered_payload() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        create_bundle_archive(&bundle, "x", &sample_artifacts(), vec![], false).unwrap();
        rebuild_with(&bundle, |entries| {
            for (name, data) in entries.iter_mut() {
                if name == "reports/disk_snapshot.json" {
                    data[0] ^= 0xFF;
                }
            }
        });

        let err = read_bundle_entry(&bundle, "reports/disk_snapshot.json").unwrap_err();
        assert!(err.to_string().contains("integrity"), "{err}");
    }
}
