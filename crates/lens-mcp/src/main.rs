use serde_json::Value;
use std::io::{self, BufRead, Read, Write};

use lens_mcp::handler::set_allowed_roots;
use lens_mcp::protocol::{get_prompt, list_prompts};
use lens_mcp::{execute_tool, list_tools, JsonRpcError, JsonRpcRequest, JsonRpcResponse};

const USAGE: &str = "\
lens-mcp — MCP (JSON-RPC 2.0 over stdio) server exposing the lens toolkit.

Usage: lens-mcp [--allow-root DIR]...

Options:
  --allow-root DIR   Restrict all tool path arguments to DIR (repeatable).
                     Paths are canonicalized; a path resolving outside every
                     allowed root is rejected with a tool error.
  -h, --help         Show this help.
";

/// Parse `--allow-root DIR` (repeatable) and `--help` without pulling in
/// clap — the binary's interface is otherwise pure stdio JSON-RPC.
fn parse_args() -> Result<Vec<std::path::PathBuf>, String> {
    let mut roots = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-root" => {
                let dir = args
                    .next()
                    .ok_or_else(|| "--allow-root requires a directory argument".to_string())?;
                roots.push(std::path::PathBuf::from(dir));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(roots)
}

fn main() -> io::Result<()> {
    match parse_args().and_then(set_allowed_roots) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut line = String::new();
    const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024 * 1024;

    loop {
        line.clear();
        let bytes_read = match (&mut reader)
            .take((MAX_REQUEST_LINE_BYTES + 1) as u64)
            .read_line(&mut line)
        {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if bytes_read > MAX_REQUEST_LINE_BYTES && !line.ends_with('\n') {
            // Drain the rest of the oversized line so framing recovers.
            let mut discard = Vec::new();
            let _ = reader.read_until(b'\n', &mut discard);
            let err_resp = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid Request: request line exceeds 8 MiB limit".to_string(),
                    data: None,
                }),
            };
            let out = serde_json::to_string(&err_resp)?;
            writeln!(stdout, "{}", out)?;
            stdout.flush()?;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                        data: None,
                    }),
                };
                let out = serde_json::to_string(&err_resp)?;
                writeln!(stdout, "{}", out)?;
                stdout.flush()?;
                continue;
            }
        };
        // `"id": null` and a missing `id` differ in JSON-RPC: the null
        // is a present id (response carries `id: null`), absence makes
        // the request a notification (no response at all).
        let raw_id = parsed.get("id").cloned();
        let id_present = raw_id.is_some();
        let req: JsonRpcRequest = match serde_json::from_value(parsed) {
            Ok(r) => r,
            Err(e) => {
                let valid_id = raw_id
                    .filter(|v| v.is_string() || v.is_number() || v.is_null())
                    .or(Some(Value::Null));
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: valid_id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32600,
                        message: format!("Invalid Request: {}", e),
                        data: None,
                    }),
                };
                let out = serde_json::to_string(&err_resp)?;
                writeln!(stdout, "{}", out)?;
                stdout.flush()?;
                continue;
            }
        };

        let resp = handle_request(&req, id_present);
        if let Some(r) = resp {
            let out = serde_json::to_string(&r)?;
            writeln!(stdout, "{}", out)?;
            stdout.flush()?;
        }
    }

    Ok(())
}

/// Protocol versions this server speaks. Echoed back when the client
/// asks for one of them; anything else (or a missing version) gets the
/// newest supported one.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn handle_request(req: &JsonRpcRequest, id_present: bool) -> Option<JsonRpcResponse> {
    // A JSON-RPC notification (no `id` member) never gets a response —
    // for any method, including tools/call and unknown methods. An
    // `id` member that IS present (even null) expects a response whose
    // `id` echoes it (`Some(Value::Null)` → `"id":null`).
    if !id_present {
        return None;
    }
    let rid = Some(req.id.clone().unwrap_or(Value::Null));
    match req.method.as_str() {
        "initialize" => {
            let protocol_version = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: rid.clone(),
                result: Some(serde_json::json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {},
                        "prompts": {}
                    },
                    "serverInfo": {
                        "name": "lens-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
                error: None,
            })
        }
        "notifications/initialized" => None,
        // MCP ping: an empty result keeps the session liveness check cheap.
        "ping" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: rid.clone(),
            result: Some(serde_json::json!({})),
            error: None,
        }),
        "tools/list" => {
            let tools = list_tools();
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: rid.clone(),
                result: Some(serde_json::json!({
                    "tools": tools
                })),
                error: None,
            })
        }
        "prompts/list" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: rid.clone(),
            result: Some(list_prompts()),
            error: None,
        }),
        "prompts/get" => {
            let params = req.params.as_ref().unwrap_or(&Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);
            match get_prompt(name, args) {
                Ok(prompt) => Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: rid.clone(),
                    result: Some(prompt),
                    error: None,
                }),
                Err(e) => Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: rid.clone(),
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: e,
                        data: None,
                    }),
                }),
            }
        }
        "tools/call" => {
            let params = req.params.as_ref().unwrap_or(&Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);

            // Unknown tool stays a JSON-RPC error; a known tool that fails
            // reports in-band (`isError: true`) so the model sees the error
            // text and can retry, per the MCP tools/call contract.
            if !list_tools().iter().any(|t| t.name == name) {
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: rid.clone(),
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: format!("Unknown tool: {}", name),
                        data: None,
                    }),
                })
            } else {
                match execute_tool(name, args) {
                    Ok(output) => {
                        // First content item is the data JSON; when the
                        // operation produced findings or incomplete
                        // evidence a second item carries them.
                        let mut content = vec![serde_json::json!({
                            "type": "text",
                            "text": output.data
                        })];
                        if let Some(f) = output.findings {
                            content.push(serde_json::json!({
                                "type": "text",
                                "text": f
                            }));
                        }
                        Some(JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id: rid.clone(),
                            result: Some(serde_json::json!({ "content": content })),
                            error: None,
                        })
                    }
                    Err(err) => Some(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: rid.clone(),
                        result: Some(serde_json::json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": lens_mcp::handler::bounded_error(err)
                                }
                            ],
                            "isError": true
                        })),
                        error: None,
                    }),
                }
            }
        }
        _ => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: rid,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", req.method),
                data: None,
            }),
        }),
    }
}
