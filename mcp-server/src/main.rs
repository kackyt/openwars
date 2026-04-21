#[allow(dead_code)]
use rmcp::handler::server::tool::Parameters;
use rmcp::model::{Implementation, ServerInfo};
use rmcp::{ServerHandler, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use engine::setup::create_world;
use bevy_ecs::world::World;
use bevy_ecs::schedule::Schedule;

#[allow(dead_code)]
struct GameState {
    pub world: World,
    pub schedule: Schedule,
}

#[derive(Clone)]
#[allow(dead_code)]
struct OpenWarsAiServer {
    pub state: Arc<Mutex<Option<GameState>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LoadMapArgs {
    pub map_name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct EvaluateBoardArgs {
    pub player_id: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct SimulateAiTurnArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct GetBoardStateArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct GetValidActionsArgs {
    pub x: u32,
    pub y: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExecuteActionArgs {}

#[tool(tool_box)]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate.")]
    async fn load_map(&self, args: Parameters<LoadMapArgs>) -> Result<String, String> {
        let (world, schedule) = create_world();
        // Here we could load a specific map by sending an event,
        // e.g., LoadMapCommand(args.0.map_name.clone()) and ticking the schedule

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState { world, schedule });

        Ok(format!("Loaded map: {}", args.0.map_name))
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(&self, args: Parameters<EvaluateBoardArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let score = engine::ai::eval::evaluate_board(&mut state.world, engine::components::PlayerId(args.0.player_id));
            Ok(format!("Board evaluation for player {}: {}", args.0.player_id, score))
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns valid actions for a unit at a given coordinate.")]
    async fn get_valid_actions(&self, _args: Parameters<GetValidActionsArgs>) -> Result<String, String> {
        let state_lock = self.state.lock().await;
        if let Some(_state) = state_lock.as_ref() {
            Ok("[]".into())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns the current state of the board.")]
    async fn get_board_state(&self, _args: Parameters<GetBoardStateArgs>) -> Result<String, String> {
        let state_lock = self.state.lock().await;
        if let Some(_state) = state_lock.as_ref() {
            Ok("{}".into())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Simulates an AI turn using the AI engine logic.")]
    async fn simulate_ai_turn(&self, _args: Parameters<SimulateAiTurnArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            state.schedule.run(&mut state.world);
            Ok("Simulated AI turn".into())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Executes an action.")]
    async fn execute_action(&self, _args: Parameters<ExecuteActionArgs>) -> Result<String, String> {
        Ok("Executed action".into())
    }
}

impl ServerHandler for OpenWarsAiServer {
    rmcp::tool_box!(@derive);

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: rmcp::model::ServerCapabilities::builder().enable_tools().build(),
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

    let server = OpenWarsAiServer {
        state: Arc::new(Mutex::new(None)),
    };

    let running_service = serve_server(server, stdio()).await?;
    running_service.waiting().await?;

    Ok(())
}
