use serde_json::Value;
use std::io::{self, BufRead, Write};

use lens_mcp::{execute_tool, list_tools, JsonRpcError, JsonRpcRequest, JsonRpcResponse};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
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

        let resp = handle_request(&req);
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

fn handle_request(req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
    // A JSON-RPC notification (no `id`) never gets a response — for any
    // method, including tools/call and unknown methods.
    req.id.as_ref()?;
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
                id: req.id.clone(),
                result: Some(serde_json::json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {}
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
            id: req.id.clone(),
            result: Some(serde_json::json!({})),
            error: None,
        }),
        "tools/list" => {
            let tools = list_tools();
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id.clone(),
                result: Some(serde_json::json!({
                    "tools": tools
                })),
                error: None,
            })
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
                    id: req.id.clone(),
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
                            id: req.id.clone(),
                            result: Some(serde_json::json!({ "content": content })),
                            error: None,
                        })
                    }
                    Err(err) => Some(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id.clone(),
                        result: Some(serde_json::json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": err
                                }
                            ],
                            "isError": true
                        })),
                        error: None,
                    }),
                }
            }
        }
        _ => req.id.as_ref().map(|id| JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(id.clone()),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", req.method),
                data: None,
            }),
        }),
    }
}
