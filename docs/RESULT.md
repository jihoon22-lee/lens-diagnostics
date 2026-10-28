# Lens result envelope v1

`lens --format envelope <report command>` opts into `schema_id: "lens.result"`, integer `schema_version: 1`. Default JSON and domain schemas are unchanged. See [fixture](fixtures/lens-result-v1.json) and [JSON Schema](schemas/lens-result-v1.schema.json).

- `producer`: name `lens` and package version, independently versioned from the envelope.
- `run_id`: caller-provided `IDK_RUN_ID`, otherwise null. No implicit result file or events file writes.
- `execution`: argv (including executable), cwd, and decision exit_code (0 clean, 1 findings/incomplete). Consumers must separately preserve actual process exit, stderr and raw bytes. Operational errors exit 2 and may have no envelope.
- `source`: kind `command` and argv identity. This identifies the requested operation, not a content digest, host identity or immutable source snapshot. Domain data and bundle manifests carry additional evidence identity where available.
- `complete`: completeness of the requested analysis evidence, independent of finding severity and transport pagination. True is never a claim of exhaustive system coverage or compiler equivalence.
- `findings`: severity info/warn/fail, stable code, message. There is no general path/line contract; consumers must not invent location evidence from message text.
- `diagnostics`: operational and scope explanations; `data`: the existing command JSON payload.

Reports support this mode. Interactive TUI, completion, trash and `log filter --follow` reject it explicitly. Finite log inspect/filter/stats/diff support it; streaming log filter retains text/JSONL. MCP retains domain JSON plus a separate findings metadata item; pagination totals/flags describe response trimming independently of analysis `complete`.

This fixture is synthetic contract evidence, not a claim that a host check ran. Unknown fields must be tolerated; unknown schema versions must not silently be interpreted as v1. argv may contain user-supplied sensitive values: consumers choose storage and redaction policies explicitly.
