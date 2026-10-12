use memmap2::Mmap;
use std::borrow::Cow;
use std::io::Read;
use std::path::Path;

use lens_core::{LensError, Result, SafeInput};

/// Cap on bytes read from decompressed/piped sources — same order of
/// magnitude as the bundle decompression cap in lens-core.
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub offset: usize,
    pub length: usize,
}

/// The line store backs the indexer: memory-mapped for regular files
/// and for `.gz`/stdin input spooled to a temp file; heap-owned only
/// for small inline buffers (`from_bytes`, empty input).
enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    fn data(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => &m[..],
            Backing::Owned(v) => &v[..],
        }
    }
}

pub struct LogIndexer {
    data: Backing,
    spans: Vec<LineSpan>,
    /// Number of indexed lines whose bytes are not valid UTF-8.
    lossy_lines: usize,
}

impl LogIndexer {
    /// Open a log file. `.gz` files are decompressed through a bounded
    /// reader (decompression bombs error out rather than expanding
    /// without limit); everything else is memory-mapped via SafeInput.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if path.extension().map(|e| e == "gz").unwrap_or(false) {
            let safe_input = SafeInput::open(path)?;
            let decoder = flate2::read::GzDecoder::new(safe_input.file);
            return Self::from_reader(decoder, &path.to_string_lossy());
        }
        let safe_input = SafeInput::open(path)?;
        let mmap = safe_input.mmap()?;
        Ok(Self::from_backing(Backing::Mapped(mmap)))
    }

    /// Index a stream (e.g. stdin for `-` input, or a `.gz` decoder):
    /// the bytes are spooled into an unnamed temp file (deleted on
    /// drop) under the same decompression cap, then memory-mapped, so
    /// a multi-hundred-MiB decompressed stream does not live in the
    /// process heap.
    pub fn from_reader<R: Read>(reader: R, source: &str) -> Result<Self> {
        let backing = spool_to_temp_mmap(reader, Path::new(source))?;
        Ok(Self::from_backing(backing))
    }

    /// Index already-materialized bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(data: Backing) -> Self {
        let bytes = data.data();
        let mut spans = Vec::with_capacity(bytes.len() / 64);
        let mut line_start = 0;

        for idx in memchr::memchr_iter(b'\n', bytes) {
            let mut line_end = idx;
            // Strip trailing \r
            if line_end > line_start && bytes[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            spans.push(LineSpan {
                offset: line_start,
                length: line_end - line_start,
            });
            line_start = idx + 1;
        }

        // Remaining bytes without trailing newline
        if line_start < bytes.len() {
            let mut line_end = bytes.len();
            if line_end > line_start && bytes[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            spans.push(LineSpan {
                offset: line_start,
                length: line_end - line_start,
            });
        }

        // Fast path: one whole-buffer UTF-8 check instead of a per-line
        // one — the overwhelmingly common case is fully valid input.
        let lossy_lines = if std::str::from_utf8(bytes).is_ok() {
            0
        } else {
            spans
                .iter()
                .filter(|s| std::str::from_utf8(&bytes[s.offset..s.offset + s.length]).is_err())
                .count()
        };

        Self {
            data,
            spans,
            lossy_lines,
        }
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Lines containing invalid UTF-8 — they are still indexed and can be
    /// retrieved lossy via `get_line_lossy`.
    pub fn lossy_lines(&self) -> usize {
        self.lossy_lines
    }

    /// Strict UTF-8 view: `None` for non-UTF-8 lines.
    pub fn get_line(&self, index: usize) -> Option<&str> {
        let span = self.spans.get(index)?;
        let bytes = &self.data.data()[span.offset..span.offset + span.length];
        std::str::from_utf8(bytes).ok()
    }

    /// Lossy view: invalid UTF-8 bytes become U+FFFD so lines are never
    /// silently skipped.
    pub fn get_line_lossy(&self, index: usize) -> Option<Cow<'_, str>> {
        let span = self.spans.get(index)?;
        let bytes = &self.data.data()[span.offset..span.offset + span.length];
        Some(String::from_utf8_lossy(bytes))
    }

    pub fn get_raw_bytes(&self, index: usize) -> Option<&[u8]> {
        let span = self.spans.get(index)?;
        Some(&self.data.data()[span.offset..span.offset + span.length])
    }

    /// Byte offset where `index`'s line starts in the backing buffer —
    /// used by `--follow` to resume exactly at an unterminated tail.
    pub fn line_offset(&self, index: usize) -> Option<usize> {
        self.spans.get(index).map(|s| s.offset)
    }

    /// The full backing bytes — decoded content for `.gz`/stdin inputs.
    pub fn as_bytes(&self) -> &[u8] {
        self.data.data()
    }
}

/// Spool `reader` into an unnamed temp file (unlinked at creation,
/// deleted on drop) and mmap it. Fails with `LimitExceeded` once the
/// stream produces more than `MAX_UNPACKED_BYTES`.
fn spool_to_temp_mmap<R: Read>(reader: R, source: &Path) -> Result<Backing> {
    let mut tmp = tempfile::tempfile().map_err(|e| LensError::Io {
        path: source.to_path_buf(),
        source: e,
    })?;
    let mut limited = reader.take(MAX_UNPACKED_BYTES + 1);
    let written = std::io::copy(&mut limited, &mut tmp).map_err(|e| LensError::Io {
        path: source.to_path_buf(),
        source: e,
    })?;
    if written > MAX_UNPACKED_BYTES {
        return Err(LensError::LimitExceeded {
            message: format!(
                "{:?} exceeds {} bytes after decompression",
                source, MAX_UNPACKED_BYTES
            ),
        });
    }
    if written == 0 {
        // mmap of a zero-length file fails; an empty log has no lines.
        return Ok(Backing::Owned(Vec::new()));
    }
    // SAFETY: the temp file is a regular file this process just created
    // and wrote; it is unlinked on creation so nobody else can modify
    // it under the map.
    let mmap = unsafe {
        memmap2::MmapOptions::new()
            .map(&tmp)
            .map_err(|e| LensError::Io {
                path: source.to_path_buf(),
                source: e,
            })?
    };
    Ok(Backing::Mapped(mmap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lossy_utf8_lines_indexed_and_counted() {
        let mut bytes = b"clean one\n".to_vec();
        bytes.extend_from_slice(b"bad \xff\xfe line\n");
        bytes.extend_from_slice(b"clean two");
        let idx = LogIndexer::from_bytes(bytes);
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.lossy_lines(), 1);
        // Strict view rejects the lossy line; lossy view keeps it.
        assert!(idx.get_line(1).is_none());
        assert_eq!(idx.get_line(0), Some("clean one"));
        assert!(idx.get_line_lossy(1).unwrap().contains("bad"));
    }

    #[test]
    fn test_gzip_input_decompresses() {
        let dir = tempfile::Builder::new()
            .prefix("lenslog-")
            .tempdir()
            .unwrap();
        let dir = dir.path();
        let gz_path = dir.join("app.log.gz");
        {
            use std::io::Write;
            let f = std::fs::File::create(&gz_path).unwrap();
            let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            enc.write_all(b"[INFO] a\n[ERROR] b\n").unwrap();
            enc.finish().unwrap();
        }
        let idx = LogIndexer::open(&gz_path).unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.get_line(1), Some("[ERROR] b"));
    }

    #[test]
    fn test_from_reader_stdin_path() {
        let idx = LogIndexer::from_reader(&b"x\ny\n"[..], "-").unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.get_line(0), Some("x"));
    }
}
