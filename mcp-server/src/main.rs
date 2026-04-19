use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

#[derive(Serialize, Deserialize, Debug)]
struct Request {
    jsonrpc: String,
    id: Option<u64>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    jsonrpc: String,
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ErrorResponse {
    code: i32,
    message: String,
}

fn handle_request(req: Request) -> Response {
    let mut result = None;
    let mut error = None;

    match req.method.as_str() {
        "initialize" => {
            // Real MCP expects an initialize response.
            result = Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "openwars-mcp",
                    "version": "1.0.0"
                }
            }));
        }
        "tools/list" => {
            result = Some(serde_json::json!({
                "tools": [
                    {
                        "name": "get_board_state",
                        "description": "Returns the current state of the board.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "evaluate_board",
                        "description": "Evaluates the board.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "get_valid_actions",
                        "description": "Returns valid actions for a unit.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "simulate_ai_turn",
                        "description": "Simulates an AI turn.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "execute_action",
                        "description": "Executes an action.",
                        "inputSchema": { "type": "object", "properties": {} }
                    }
                ]
            }));
        }
        "tools/call" => {
            // For now, return a dummy success string based on the tool.
            let tool_name = req
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            match tool_name {
                "get_board_state" => {
                    result = Some(
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"status\": \"ok\", \"units\": []}" }] }),
                    );
                }
                "evaluate_board" => {
                    result = Some(
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"score\": 0}" }] }),
                    );
                }
                "get_valid_actions" => {
                    result = Some(
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"actions\": []}" }] }),
                    );
                }
                "simulate_ai_turn" => {
                    result = Some(
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"planned_actions\": []}" }] }),
                    );
                }
                "execute_action" => {
                    result = Some(
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"status\": \"success\"}" }] }),
                    );
                }
                _ => {
                    error = Some(ErrorResponse {
                        code: -32601,
                        message: "Tool not found".to_string(),
                    });
                }
            }
        }
        _ => {
            // Fallback for standard JSON-RPC
            error = Some(ErrorResponse {
                code: -32601,
                message: "Method not found".to_string(),
            });
        }
    }

    Response {
        jsonrpc: "2.0".to_string(),
        id: req.id,
        result,
        error,
    }
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let res = handle_request(req);
                if let Ok(json) = serde_json::to_string(&res) {
                    println!("{}", json);
                    let _ = stdout.flush();
                }
            }
            Err(e) => {
                let err_res = Response {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(ErrorResponse {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                    }),
                };
                if let Ok(json) = serde_json::to_string(&err_res) {
                    println!("{}", json);
                    let _ = stdout.flush();
                }
            }
        }
    }
}
