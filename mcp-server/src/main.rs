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

use engine::components::{GridPosition, Health, PlayerId, Faction, UnitStats, Property};
use engine::resources::{GameRng, MatchState, Players, Player, Map, UnitRegistry, DamageChart, Terrain, GridTopology};
use engine::resources::master_data::{MasterDataRegistry, UnitName};
use bevy_ecs::prelude::Entity;

#[tool(tool_box)]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate.")]
    async fn load_map(&self, args: Parameters<LoadMapArgs>) -> Result<String, String> {
        let registry = MasterDataRegistry::load().map_err(|e| format!("Failed to load master data: {}", e))?;
        let map_data = registry.get_map(&args.0.map_name).ok_or_else(|| format!("Map '{}' not found", args.0.map_name))?;

        let (mut world, schedule) = create_world();

        // 1. Setup Resources from Registry
        let mut damage_chart = DamageChart::new();
        for (unit_name, record) in &registry.units {
            let att_type = registry.unit_type_for_name(&unit_name.0).map_err(|e| e.to_string())?;
            if let Some(w1_name) = &record.weapon1 {
                if let Some(w) = registry.weapons.get(&UnitName(w1_name.clone())) {
                    for (def_name, dmg) in &w.damages {
                        let def_type = registry.unit_type_for_name(def_name).map_err(|e| e.to_string())?;
                        damage_chart.insert_damage(att_type, def_type, *dmg);
                    }
                }
            }
            if let Some(w2_name) = &record.weapon2 {
                if let Some(w) = registry.weapons.get(&UnitName(w2_name.clone())) {
                    for (def_name, dmg) in &w.damages {
                        let def_type = registry.unit_type_for_name(def_name).map_err(|e| e.to_string())?;
                        damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
                    }
                }
            }
        }
        world.insert_resource(damage_chart);

        let mut unit_registry_map = std::collections::HashMap::new();
        for name in registry.units.keys() {
            let stats = registry.create_unit_stats(name).map_err(|e| e.to_string())?;
            unit_registry_map.insert(stats.unit_type, stats);
        }
        world.insert_resource(UnitRegistry(unit_registry_map));
        world.insert_resource(GameRng::default());
        world.insert_resource(registry.clone());

        // 2. Setup Map and Properties
        let width = map_data.width;
        let height = map_data.height;
        let mut ecs_map = Map::new(width, height, Terrain::Plains, GridTopology::Square);
        let mut players_in_map = std::collections::HashSet::new();

        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = map_data.get_cell(x, y) {
                    let terrain = registry.terrain_from_id(cell.terrain_id).map_err(|e| e.to_string())?;
                    let _ = ecs_map.set_terrain(x, y, terrain);
                    if cell.player_id != 0 {
                        players_in_map.insert(cell.player_id);
                    }

                    // Properties
                    let landscape_name = terrain.as_str();
                    let durability = registry.landscape_durability(landscape_name);
                    if durability > 0 {
                        let owner = if cell.player_id == 0 { None } else { Some(PlayerId(cell.player_id)) };
                        world.spawn((
                            GridPosition { x, y },
                            Property::new(terrain, owner, durability),
                        ));
                    }
                }
            }
        }
        world.insert_resource(ecs_map);
        world.insert_resource(MatchState::default());

        // 3. Players
        players_in_map.insert(1);
        players_in_map.insert(2);
        let mut player_list = vec![];
        for &pid in &players_in_map {
            let mut income = 0;
            for y in 0..height {
                for x in 0..width {
                    if let Some(cell) = map_data.get_cell(x, y) && cell.player_id == pid {
                        let landscape = registry.get_landscape(cell.terrain_id).ok_or_else(|| "Unknown terrain".to_string())?;
                        income += registry.landscape_income(&landscape.name);
                    }
                }
            }
            let mut p = Player::new(pid, format!("Player {}", pid));
            p.funds = income;
            player_list.push(p);
        }
        player_list.sort_by_key(|p| p.id.0);
        world.insert_resource(Players(player_list));

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState { world, schedule });

        Ok(format!("Loaded map: {}", args.0.map_name))
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(&self, args: Parameters<EvaluateBoardArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let score = engine::ai::eval::evaluate_board(&mut state.world, PlayerId(args.0.player_id));
            Ok(serde_json::json!({
                "player_id": args.0.player_id,
                "score": score
            }).to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns valid actions for a unit at a given coordinate.")]
    async fn get_valid_actions(&self, args: Parameters<GetValidActionsArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            // Find unit at (x, y)
            let mut unit_entity = None;
            let mut query = world.query::<(Entity, &GridPosition)>();
            for (entity, pos) in query.iter(world) {
                if pos.x == args.0.x as usize && pos.y == args.0.y as usize {
                    if world.get::<UnitStats>(entity).is_some() {
                        unit_entity = Some(entity);
                        break;
                    }
                }
            }

            if let Some(entity) = unit_entity {
                let actions = engine::systems::get_available_actions(world, entity, false);
                Ok(serde_json::to_string(&actions).map_err(|e| e.to_string())?)
            } else {
                Ok("[]".into())
            }
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns the current state of the board.")]
    async fn get_board_state(&self, _args: Parameters<GetBoardStateArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let mut units = Vec::new();
            let mut query = world.query::<(Entity, &GridPosition, &Faction, &Health, &UnitStats)>();
            for (entity, pos, faction, health, stats) in query.iter(world) {
                units.push(serde_json::json!({
                    "entity": format!("{:?}", entity),
                    "x": pos.x,
                    "y": pos.y,
                    "faction": faction.0.0,
                    "hp": health.current,
                    "unit_type": format!("{:?}", stats.unit_type)
                }));
            }

            let mut properties = Vec::new();
            let mut prop_query = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in prop_query.iter(world) {
                properties.push(serde_json::json!({
                    "x": pos.x,
                    "y": pos.y,
                    "owner": prop.owner_id.map(|p| p.0),
                    "terrain": prop.terrain.as_str()
                }));
            }

            Ok(serde_json::json!({
                "units": units,
                "properties": properties
            }).to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Simulates an AI turn using the AI engine logic.")]
    async fn simulate_ai_turn(&self, _args: Parameters<SimulateAiTurnArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let (active_player_id, active_player_index) = {
                let ms = state.world.get_resource::<MatchState>().ok_or("No MatchState")?;
                let players = state.world.get_resource::<Players>().ok_or("No Players")?;
                let p = players.0.get(ms.active_player_index.0).ok_or("No active player")?;
                (p.id, ms.active_player_index)
            };

            let before_score = engine::ai::eval::evaluate_board(&mut state.world, active_player_id);
            let action_taken = engine::ai::engine::execute_ai_turn(&mut state.world, active_player_id);
            
            // 重要: AIの行動(Event)を発行したあとは、システムを実行して状態を更新する必要がある
            state.schedule.run(&mut state.world);
            
            let after_score = engine::ai::eval::evaluate_board(&mut state.world, active_player_id);

            Ok(serde_json::json!({
                "action_taken": action_taken,
                "player_id": active_player_id.0,
                "player_index": active_player_index.0,
                "before_score": before_score,
                "after_score": after_score
            }).to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Executes an action.")]
    async fn execute_action(&self, _args: Parameters<ExecuteActionArgs>) -> Result<String, String> {
        Ok("Executed action (Stated stub, please use JSON params for specific commands)".into())
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
