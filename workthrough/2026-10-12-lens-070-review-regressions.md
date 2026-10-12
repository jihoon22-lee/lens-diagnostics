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
- `cargo test --workspace`: 313 tests passed, including actual MCP subprocess
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
