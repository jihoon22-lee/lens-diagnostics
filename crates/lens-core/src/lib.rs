//! # Lens Core (`lens-core`)
//!
//! Foundation library for the Unified Systems Diagnostics and Forensic Platform.
//!
//! This crate establishes the shared invariants across all diagnostic domain modules:
//! - **Identity ([`identity`])**: POSIX file metadata, Inode tracking, and TOCTOU race condition defense.
//! - **Hash ([`hash`])**: Standard SHA-256 hashing and streaming digest calculations.
//! - **Diff ([`diff`])**: Conservative 3-state compatibility model (`Compatible`, `Incompatible`, `Uncertain`).
//! - **Evidence ([`evidence`])**: Source provenance, exact observation coordinates, and bounded diagnostic collectors.
//! - **Bundle ([`bundle`])**: The canonical `.lens` archive format shared by create/inspect/verify.
//! - **JSON ([`json`])**: Deterministic canonical JSON serialization with guaranteed key sorting.
//! - **Time ([`time`])**: Shared ISO-8601 timestamp helpers.
//! - **Error ([`error`])**: Unified typed error handling.

pub mod bundle;
pub mod diff;
pub mod error;
pub mod evidence;
pub mod glob;
pub mod hash;
pub mod identity;
pub mod json;
pub mod read_policy;
pub mod redact;
pub mod time;

pub use bundle::{
    create_bundle_archive, extract_bundle, inspect_bundle, normalize_entry_name, read_bundle_entry,
    verify_bundle_archive, BundleArtifact, BundleInspection, BundleManifest,
    BundleVerificationReport, BUNDLE_SCHEMA_V2,
};
pub use diff::{Compatibility, SetDiff};
pub use error::{LensError, Result};
pub use evidence::{BoundedCollector, Diagnostic, Evidence, Source};
pub use hash::{digest_bytes, digest_file, digest_reader, IncrementalHasher};
pub use identity::{FileIdentity, SafeInput};
pub use json::{to_deterministic_pretty, to_deterministic_string};
pub use redact::{redact_bytes, redact_str};
