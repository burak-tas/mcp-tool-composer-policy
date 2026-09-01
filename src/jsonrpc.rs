//! Minimal JSON-RPC 2.0 envelope types for MCP Streamable HTTP transport.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Error codes from the MCP / JSON-RPC spec.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// Inbound JSON-RPC request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: Option<String>,
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// Outbound JSON-RPC response (success or error).
#[derive(Debug, Serialize)]
pub struct JsonRpcOutbound {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub fn success_response(id: Option<Value>, result: Value) -> JsonRpcOutbound {
    JsonRpcOutbound {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

pub fn error_response(id: Option<Value>, code: i32, message: impl Into<String>) -> JsonRpcOutbound {
    JsonRpcOutbound {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

pub fn error_response_with_data(
    id: Option<Value>,
    code: i32,
    message: impl Into<String>,
    data: Value,
) -> JsonRpcOutbound {
    JsonRpcOutbound {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: Some(data),
        }),
    }
}
