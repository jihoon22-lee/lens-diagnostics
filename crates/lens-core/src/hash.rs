use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{LensError, Result};

pub fn digest_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

pub fn digest_reader<R: Read>(mut reader: R) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn digest_file<P: AsRef<Path>>(path: P) -> Result<String> {
    let file = crate::read_policy::open(path.as_ref()).map_err(|e| LensError::Io {
        path: path.as_ref().to_path_buf(),
        source: e,
    })?;
    let reader = BufReader::new(file);
    digest_reader(reader).map_err(|e| LensError::Io {
        path: path.as_ref().to_path_buf(),
        source: e,
    })
}

#[derive(Default)]
pub struct IncrementalHasher {
    inner: Sha256,
}

impl IncrementalHasher {
    pub fn new() -> Self {
        Self {
            inner: Sha256::new(),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> String {
        format!("{:x}", self.inner.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_digest() {
        assert_eq!(
            digest_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_known_string() {
        assert_eq!(
            digest_bytes(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
