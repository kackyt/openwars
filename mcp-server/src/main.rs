use rmcp::model::{Implementation, ServerInfo};
use rmcp::{ServerHandler, tool};

#[derive(Clone)]
struct OpenWarsAiServer;

#[tool]
impl OpenWarsAiServer {
    #[tool(description = "Returns the current state of the board.")]
    async fn get_board_state(&self) -> Result<String, String> {
        Err("Tool 'get_board_state' is not wired to the engine yet".into())
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(&self) -> Result<String, String> {
        Err("Tool 'evaluate_board' is not wired to the engine yet".into())
    }

    #[tool(description = "Returns valid actions for a unit.")]
    async fn get_valid_actions(&self) -> Result<String, String> {
        Err("Tool 'get_valid_actions' is not wired to the engine yet".into())
    }

    #[tool(description = "Simulates an AI turn.")]
    async fn simulate_ai_turn(&self) -> Result<String, String> {
        Err("Tool 'simulate_ai_turn' is not wired to the engine yet".into())
    }

    #[tool(description = "Executes an action.")]
    async fn execute_action(&self) -> Result<String, String> {
        Err("Tool 'execute_action' is not wired to the engine yet".into())
    }
}

impl ServerHandler for OpenWarsAiServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "openwars-mcp".into(),
                version: "1.0.0".into(),
            },
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::serve_server;
    use rmcp::transport::io::stdio;

    let server = OpenWarsAiServer;

    serve_server(server, stdio()).await?;

    Ok(())
}
