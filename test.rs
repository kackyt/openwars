use rmcp::{tool, ServerHandler};
use rmcp::model::ServerInfo;
use rmcp::handler::server::tool::{Parameters};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct LoadMapArgs { map_name: String }

#[derive(Clone)]
struct OpenWarsAiServer;

#[tool]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate. Example maps: 'g1_01', 'g1_02'")]
    async fn load_map(&self, args: Parameters<LoadMapArgs>) -> Result<String, String> {
        Err("Tool 'load_map' is not wired to the engine yet".into())
    }
}
