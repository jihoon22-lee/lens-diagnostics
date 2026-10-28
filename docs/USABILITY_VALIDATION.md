# Usability validation: scale and cancellation

These are local Linux x86_64 observations on 2026-10-28, using debug binaries,
not cross-platform performance guarantees. Baseline source is
`7fb230aab8257c1511de546d08070e8f2e08981b` (same implementation as v0.10.0).
The fixture `scripts/fixtures/usability_probe.rs` retains that baseline `stats`
function beside calls to the current implementation so both run with the same
compiler, dependencies, input and build mode. It is a measurement harness, not
an installed binary.

## Reproduction

Copy the harness to `crates/lens-ops/examples/scale_probe.rs` and run:

```sh
flock /tmp/paseo-products-20261028-heavy.lock cargo build -p lens-ops --example scale_probe -j 2
```

Create a file containing exactly 1,000,000 repetitions of
`2026-10-09T12:00:00Z WARN repeated observation\n` (47,000,000 bytes).
For each mode `old`, `new`, and `index`, run under the same shared lock:

```sh
/usr/bin/time -f 'peak_kib=%M elapsed=%e' target/debug/examples/scale_probe MODE INPUT
```

Compare old/new stdout byte-for-byte with `cmp`. For cancellation, create a
fresh directory with 20,000 empty files, then run the same harness with mode
`cancel` and that directory. It starts a real scanner, waits 2 ms, sets the
cancellation token and joins the worker, printing elapsed time and partial
result diagnostics. Remove the temporary example, inputs, outputs and directory
when finished. Do not remove pre-existing build caches.

## Observed results

| Run | Peak RSS (KiB) | Elapsed (seconds) | Result |
|---|---:|---:|---|
| Baseline stats | 80,976 | 9.27 | 1,000,000 matched lines |
| Incremental stats | 65,280 | 8.67 | Byte-identical JSON |
| Index only | 65,288 | 0.31 | 1,000,000 indexed lines |

This fixture reduced observed peak RSS by 15,696 KiB (19.4%). The line index
remains proportional to input lines: `LineSpan` is 16 bytes on this target,
plus vector spare capacity, mapped input pages, and process overhead. This is
not a constant-memory log implementation. Histogram storage now scales with
occupied output buckets rather than matched observations. Unusually long
spans still produce many daily buckets; machine output is not silently capped.

Real cancellation took 0.264 ms from token set to worker join (2.438 ms total),
with one retained entry and diagnostic `Scan cancelled; retained tree is partial
evidence`. Cancellation happened while enumerating the large directory. This
does not establish a latency bound for stalled filesystems or kernel I/O.

A deterministic TUI regression holds one worker alive while issuing 1,000
navigation requests, verifies a single latest pending request, cancels it, and
then successfully scans a new target. The implementation starts no second worker
until the old worker finishes; event-loop polling never joins an unfinished
worker. Existing stale-result and navigation tests pass.

Adaptive histogram regression compares automatic and fixed-width complete JSON
for repeated, out-of-order timestamps crossing all width transitions and the
Unix epoch. Existing negative timestamp, unknown timestamp, year override,
lossy input and histogram tests pass. Log comparison now explicitly reports
changed observed time bounds and unknown/incomplete duration, while retaining
raw count deltas and their existing findings.

Validation executed under the shared heavy lock: `cargo test -p lens-tui -p
lens-ops -j 2 --lib` passed 64 ops and 7 TUI tests at that checkpoint; the later
added `log::tests::diff_explains_changed_and_unknown_time_scope` passed separately.
Full workspace/release acceptance belongs to the main execution ledger.

## Real binary acceptance

On 2026-10-28, `.github/scripts/acceptance_smoke.py target/debug` ran under
`flock /tmp/paseo-products-20261028-heavy.lock`. This reusable Linux smoke
requires Python and `jsonschema`; these are validation dependencies, not native
binary runtime dependencies. It creates and removes its own temporary fixtures.
The working implementation still reported `lens 0.10.0` at this checkpoint;
this is pre-release debug evidence, not a claim about published asset bytes.

| Surface | Actual result |
|---|---|
| disk, ABI, log, JUnit, trace, sys, build, env | Real binary envelope output passed v1 JSON Schema; exit 0, complete true |
| net | Current namespace observation returned exit 1, complete false; not treated as full host coverage |
| doctor | Synthetic root inspection passed; exit 0, complete true describes only available checks |
| bundle create/verify/inspect and self diff | Real fixtures succeeded with valid envelopes |
| TUI | Real PTY rendered ANSI screen and accepted `q`, exit 0 |
| MCP | stdio initialize and tools/list succeeded; out-of-root read denied, in-root log read succeeded |
| MCP pagination | doctor limit 1 returned one check and larger total in `_pagination` |
| Consumer corpus | All four envelope fixtures validated against v1 JSON Schema; exit-2 capture validated separately |

JUnit deliberately contains a failed test: its exit 0 under the default fail
threshold is expected because test failures produce warn findings. The script
checks decision exit-code agreement, rather than conflating test status with
Lens severity. Docker, Podman, a live systemd journal, native Windows, macOS and
other CPUs were not exercised by this smoke.

Observed debug SHA-256 at this checkpoint:

- lens: `cae14b93f34d5511866c5e1ae6b9056e4cfbf5afe263ccaf80ed5b5cb1730bba`
- lens-tui: `1323defb49d0801cc87bb923762ddad42c5fe08565fdefcd7ae759a2ea98710c`
- lens-mcp: `a8338ca07d0ab502711803b4c17503bef7de0fc7751337235f5dccf21f60c5f6`

Release acceptance must rerun this script against the extracted candidate/public
binaries and separately verify archive checksums and source identity.

## Integrated implementation candidate

Implementation PR [#156](https://github.com/jihoon22-lee/lens-diagnostics/pull/156)
passed GitHub Merge Gate and merged as `a62b51e`.
Local workspace validation passed 430 tests, rustfmt, and Clippy with warnings denied.
Documentation consistency, six release-flow tests and two installer tests passed.
The Linux musl build passed the same real-binary acceptance script, including TUI
PTY and MCP permission/pagination checks. All three executables had no ELF
interpreter. This candidate still used version 0.10.0; the separate 0.11.0 release
bump and public-asset verification follow.

## Published v0.11.0 acceptance

Release PR [#157](https://github.com/jihoon22-lee/lens-diagnostics/pull/157) passed
Merge Gate. Tag `v0.11.0` and `BUILD.json.source_sha` both identify
`9d9f6ff62cfa6d6fe68fc3dca38d66018586af7d`.
The [release run](https://github.com/jihoon22-lee/lens-diagnostics/actions/runs/40691267570)
built the explicit Linux musl target, uploaded draft assets, downloaded those
same assets, checked checksums/source/binary digests and execution, then published.
The [public release](https://github.com/jihoon22-lee/lens-diagnostics/releases/tag/v0.11.0)
was independently downloaded again on 2026-10-28. Anonymous HTTPS download of
the canonical archive was byte-identical to the release API download. Both
canonical and `linux-x86_64` alias archives have the same SHA-256:

`dc841437d5d750078fcf3326c44816886cc25788a118e96c40f5073e5478645b`

Published executable SHA-256 (verified against archive `BUILD.json`, installed
bytes, and subsequent extracted copies):

| Binary | SHA-256 |
|---|---|
| lens | `88b55d57235312bbdaaf93b1320a96243607bd5faf04b48311a5524f12a92fdc` |
| lens-tui | `c222585546ba0ad24102be724c07c8f891b5a32343f0f9a0dab4bb47599257b2` |
| lens-mcp | `7cfa8f91c08a75fc31f94c359f904ab28172823f82b90617889b72bc4253c3da` |

All downloaded checksums passed, including installer and release notes. The
published binaries use `rustc 1.99.1 (58cf8f9c3 2026-10-13)` and have no ELF
interpreter. Local build digests are separate measurements; reproducible equality
between different local/CI compiler environments is not claimed.

Actual public installation/execution checks:

- Fresh prefix containing spaces installed all three executables successfully.
- Reinstall without `--force` failed and preserved every installed digest;
  explicit `--force` update succeeded.
- `verify_release.py` confirmed version, source SHA and executable digests.
- `acceptance_smoke.py` against that installed prefix passed the full surface
  table above: all nine domains, doctor, bundle, TUI PTY, MCP root restrictions
  and pagination, and fixed consumer corpus.
- Native archive extraction, per-binary digest comparison and `--help` for all
  three binaries passed with PATH containing only the extracted binaries, with
  no Python command available. Python is used for the validation harness and
  optional installer, not required to run the native executables.
- Installer corruption and replacement regressions passed in the two existing
  installer tests; six release-flow tests and documentation checks also passed.

Permission, malformed input, partial evidence, external adapter timeout/stream
caps and MCP boundary cases remain covered by the 430-test workspace suite.
Actual journal/Docker/Podman runtime integration, native Windows, macOS and
other CPU architectures were not executed. Net's incomplete current-namespace
observation remains incomplete; no host-wide success is inferred.
