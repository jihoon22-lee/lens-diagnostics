pub mod handler;
pub mod protocol;

pub use handler::execute_tool;
pub use protocol::{list_tools, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTool};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_tools() {
        let tools = list_tools();
        assert!(!tools.is_empty());
        assert!(tools.iter().any(|t| t.name == "lens_disk_scan"));
        assert!(tools.iter().any(|t| t.name == "lens_trace_analyze"));
    }

    #[test]
    fn test_execute_unknown_tool() {
        let res = execute_tool("nonexistent", &serde_json::json!({}));
        assert!(res.is_err());
    }

    #[test]
    fn test_execute_lens_doctor() {
        let res = execute_tool("lens_doctor", &serde_json::json!({ "root_path": "/" }));
        assert!(res.is_ok());
        let out = res.unwrap();
        let val: serde_json::Value = serde_json::from_str(&out.data).unwrap();
        assert_eq!(val["schema_version"], "lens.doctor/v1");
        assert!(val["checks"].is_array());
    }
}
