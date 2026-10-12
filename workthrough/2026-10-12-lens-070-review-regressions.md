# Lens 0.7.0 post-release review regressions

## Context and reproduction

The 06:05 UTC review of PR #144 reported two P1 and nine P2 defects.
Work started from main `50ba99179d8070c3fd80b9c21e7cd6d1a5e2c070` in an independent worktree.
The original checkout was clean; no release version or tag was changed.

Before fixes, both new P1 regressions failed on that source:

- `allow_root_rejects_descendant_symlink_reads`: an allowed directory contained
  `secret.service` pointing to an external file of the same name. The actual
  MCP stdio response returned its self-cycle with `complete=true` instead of
  rejecting the read.
- `bundle_diff_preserves_collection_failures`: two integrity-valid bundles
  recorded the same collector failure. The diff incorrectly returned complete.

## Changes

- `lens-core/src/read_policy.rs` provides an optional process-wide read policy
  installed at MCP startup. Each discovered file/directory is canonicalized at
  I/O time. A recorded denial makes the synchronous MCP call fail even if an
  optional evidence collector ordinarily recovers from I/O failures.
- Systemd loader/drop-ins, JUnit expansion, doctor, network process/socket
  readers, environment metadata and recursive build includes use that policy.
  Allowed in-root links and links between explicitly allowed roots still work.
  `lens-build` and `lens-env` now depend on the existing `lens-core` crate.
- Bundle collection records incomplete JUnit and disk evidence. Diff carries
  baseline/candidate manifest diagnostics, checks every schema-bearing section,
  and retains disk snapshot completeness even for older bundles.
- MCP includes report diagnostics, their total count and truncation state.
  `lens_log_stats` advertises and honors `include_unknown`.
- Data and findings/diagnostics each receive a budget that includes JSON string
  escaping and reserves space for both content items and the result envelope.
  Oversized arrays are paginated. If a single value is still too large, data
  becomes a bounded `_data_omitted`/`_response_clamped` summary. Metadata retains
  original totals and explicit truncation flags when items cannot fit.
- `sys diff` accepts live unit files including drop-ins. Log follow opens first
  and uses metadata from that file handle; a missing rotation replacement is
  retryable. Log stats retain zero/negative timestamps and use Euclidean bucket
  division, so 1969-12-31 23:59:59.500 belongs to the preceding minute.

## Verification

- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: 318 tests passed, including actual MCP subprocess
  tests for systemd/JUnit/doctor escapes, drop-ins, XML globs, secondary readers,
  multiple allowed roots, diagnostics and an oversized JUnit failure response.
- `cargo build --release --workspace`: passed.
- `cargo fmt --all -- --check` and documentation consistency: passed.
- Release-flow script tests: 6 passed.
- Actual release CLI: auto-detected unit-file diff returned exit 1 for a new
  cycle; truncated JUnit bundle creation and bundle diff returned exit 1;
  epoch/pre-epoch log stats produced two correct minute buckets. `lens
  --version` and `lens-tui --help` passed.
- Focused tests cover recoverable truncated JUnit, an actual disk scan capped
  at one entry, foreign section schemas, rotation with two missing-path polls,
  epoch/pre-epoch/syslog-year timestamps and individual unit-file comparison.

## Limits

The read policy checks application I/O paths; it is not an OS sandbox. Static
symlink fixtures are covered. Concurrent hostile filesystem mutation between
canonicalization and opening has not been claimed as validated. The MCP server
dispatches calls synchronously; its denial accumulator follows that contract.
Windows-native binaries and non-Linux filesystem behavior are not validated.

## PR #146 follow-up review

The first code review identified four additional P2 cases. All four new
`followup_` regressions failed on `ef1d9d7` before the follow-up fixes:

- External systemd aliases of a different basename were classified before a
  guarded read. Alias resolution is now checked, including dangling targets;
  the denial is identical whether the external target exists or not. Local
  aliases remain supported. `/dev/null` masks are an explicit fixed sentinel
  exception and are classified without opening the device.
- Disk scans exposed symlink target metadata. Target stat and directory reads
  now use the read policy, including scans performed by worker threads.
- A large caller-controlled validation error bypassed the output budget.
  In-band tool errors are now capped by encoded size with a truncation notice.
- `.automount`, `.swap`, and `.scope` were missing from the detector. All ten
  loader-supported suffixes are now exercised through detection and diff.

The initial security review completed with no findings. Follow-up changes
require a new review on their exact head before merge.

After the follow-up fixes, all 313 workspace tests, strict clippy, release
build, documentation checks and six release-flow tests passed. Actual release
CLI/MCP runs also confirmed identical denials for existing/dangling external
aliases, blocked disk target metadata, bounded large validation errors, and
successful comparisons for the three previously omitted suffixes.

## Second review: boundary prechecks and allocation limits

Three new regressions failed on `19e8b36` and passed after correction:
external drop-in directory links bypassed the boundary when dangling; an
external symlink chain ending at `/dev/null` bypassed the mask exception; and
large manifest diagnostics were cloned before response truncation.

Drop-in metadata is now policy-checked before the directory-type gate, and
only an immediate `/dev/null` target receives mask treatment. Existing and
dangling external targets return identical denials.

Bundle manifests now have a 1 MiB metadata cap enforced at the tar header,
before entry buffering/parsing, across verify/inspect/show/extract. Creation
uses a capped serializer before deterministic serialization. Bundle diff
imports at most 128 collection diagnostic messages with a shared 64 KiB
budget and at most 4 KiB per message; explicit truncation notices preserve
the incomplete-evidence status. MCP metadata serialization borrows its
inputs and writes through a capped writer rather than cloning oversized
JSON values; size estimates no longer materialize the lists.

Final local verification: 318 workspace tests, strict clippy, fmt, release
build, documentation checks and all six release-flow tests passed. Direct
release execution rejected an 8,573-byte compressed bundle containing an
8 MiB diagnostic before parsing; `/usr/bin/time` measured peak RSS 4,552 KiB
for that MCP process. All earlier release CLI/MCP smoke checks still passed.

## Third review: material fixes and explicit residual limits

Two further material regressions were reproduced on `275771f` before fixing:
single-file unit aliases were parsed as ordinary units and masks were omitted;
unknown section fields allocated an intermediate JSON tree. The latter also
failed on a deeply nested unknown extension that typed deserialization skips.

Single-file and directory inputs now share unit classification. Bundle reports
deserialize once into typed structures, then validate their schema; the small
log summary also uses a typed structure. Foreign schemas still fail closed.

All 320 workspace tests, strict clippy, fmt, release build, docs checks and six
release-flow tests passed. Actual release CLI output confirms alias/mask
classification. An 8,328-byte bundle with 4 million unknown numeric values
(8,000,310-byte section) used 298,900 KiB peak RSS before the fix and 13,384 KiB
after it. Existing release smoke regressions still pass.

Two metadata-only findings were reproduced and intentionally deferred under
the user's instruction to avoid blocking completion on minor hardening:

- An out-and-back symlink chain can reveal an external hop's existence, even
  though the content endpoint is inside an allowed root.
- Optional environment/doctor descendant existence prechecks can distinguish
  existing from dangling external targets; an attempted external content read
  is still denied.

These are acknowledged limitations, not claims of fixes or full isolation.
The guide documents them. No external content escape was observed in these
fixtures. Full per-component resolution/probe isolation remains future work;
concurrent hostile filesystem replacement and native Windows remain untested.
