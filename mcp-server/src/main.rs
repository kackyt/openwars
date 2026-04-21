#[allow(dead_code)]
use rmcp::handler::server::tool::Parameters;
use rmcp::model::{Implementation, ServerInfo};
use rmcp::{ServerHandler, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use bevy_ecs::world::World;
use bevy_ecs::schedule::Schedule;
use bevy_ecs::prelude::Entity;

#[allow(dead_code)]
struct GameState {
    pub world: World,
    pub schedule: Schedule,
}

use engine::resources::master_data::{MasterDataRegistry, UnitName};
use engine::resources::{Map, GridTopology, Terrain, Player as EnginePlayer, Players, UnitRegistry, DamageChart, GameRng, MatchState};
use engine::setup::create_world;
use engine::components::{GridPosition, Health, PlayerId, Faction, UnitStats, Property, Fuel, Ammo, HasMoved, ActionCompleted};

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
pub struct SpawnUnitArgs {
    pub x: u32,
    pub y: u32,
    pub unit_name: String,
    pub player_id: u32,
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
pub struct NextPhaseArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct ExecuteActionArgs {}

#[tool(tool_box)]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate.")]
    async fn load_map(&self, #[tool(aggr)] args: Parameters<LoadMapArgs>) -> Result<String, String> {
        let registry = MasterDataRegistry::load().map_err(|e| format!("Failed to load master data: {}", e))?;
        let map_data = registry.get_map(&args.0.map_name).ok_or_else(|| format!("Map '{}' not found", args.0.map_name))?;

        let (mut world, schedule) = create_world();

        // 1. Initialize Map resource & Properties
        let mut map_tiles = vec![Terrain::Plains; map_data.width * map_data.height];
        for y in 0..map_data.height {
            for x in 0..map_data.width {
                let cell = map_data.get_cell(x, y).unwrap();
                let terrain = registry.terrain_from_id(cell.terrain_id).map_err(|e| e.to_string())?;
                map_tiles[y * map_data.width + x] = terrain;
                
                let durability = registry.landscape_durability(terrain.as_str());
                let owner_id = if cell.player_id > 0 { Some(PlayerId(cell.player_id)) } else { None };
                
                // Spawn property entities
                world.spawn((
                    GridPosition { x, y },
                    Property::new(terrain, owner_id, durability),
                ));
            }
        }
        world.insert_resource(Map {
            width: map_data.width,
            height: map_data.height,
            tiles: map_tiles,
            topology: GridTopology::Square,
        });

        // 2. Initialize Players resource
        world.insert_resource(Players(vec![
            EnginePlayer { id: PlayerId(1), name: "Player 1".to_string(), funds: 10000 },
            EnginePlayer { id: PlayerId(2), name: "Player 2".to_string(), funds: 10000 },
        ]));

        // 3. Initialize UnitRegistry and DamageChart from MasterDataRegistry
        let mut unit_stats_map = std::collections::HashMap::new();
        for (u_name, _) in &registry.units {
            if let Ok(stats) = registry.create_unit_stats(u_name) {
                unit_stats_map.insert(stats.unit_type, stats);
            }
        }
        world.insert_resource(UnitRegistry(unit_stats_map));

        let mut damage_chart = DamageChart::new();
        for (w_name, w_rec) in &registry.weapons {
            if let Some(unit_type) = engine::resources::UnitType::from_str(&w_name.0) {
                for (def_name, &dmg) in &w_rec.damages {
                    if let Some(def_type) = engine::resources::UnitType::from_str(def_name) {
                        damage_chart.insert_damage(unit_type, def_type, dmg);
                    }
                }
            }
        }
        world.insert_resource(damage_chart);
        world.insert_resource(GameRng::default());
        world.insert_resource(MatchState::default());
        world.insert_resource(registry);

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState { world, schedule });

        Ok(format!("Loaded map: {}", args.0.map_name))
    }

    #[tool(description = "Spawns a specific unit at a given coordinate.")]
    async fn spawn_unit(&self, #[tool(aggr)] args: Parameters<SpawnUnitArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let registry = world.resource::<MasterDataRegistry>().clone();
            
            let unit_name = UnitName(args.0.unit_name.clone());
            let stats = registry.create_unit_stats(&unit_name).map_err(|e| format!("Failed to create unit stats: {}", e))?;
            
            world.spawn((
                GridPosition { x: args.0.x as usize, y: args.0.y as usize },
                Faction(PlayerId(args.0.player_id)),
                stats.clone(),
                Health { current: 100, max: 100 },
                Fuel { current: stats.max_fuel, max: stats.max_fuel },
                Ammo { 
                    ammo1: stats.max_ammo1, max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2, max_ammo2: stats.max_ammo2
                },
                HasMoved(false),
                ActionCompleted(false),
            ));
            
            Ok(format!("Spawned {} at ({}, {}) for player {}", args.0.unit_name, args.0.x, args.0.y, args.0.player_id))
        } else {
            Err("No map loaded".into())
        }
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(&self, #[tool(aggr)] args: Parameters<EvaluateBoardArgs>) -> Result<String, String> {
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
    async fn get_valid_actions(&self, #[tool(aggr)] args: Parameters<GetValidActionsArgs>) -> Result<String, String> {
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
    async fn get_board_state(&self, #[tool(aggr)] _args: Parameters<GetBoardStateArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            
            let mut properties = vec![];
            let mut prop_query = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in prop_query.iter(world) {
                properties.push(serde_json::json!({
                    "x": pos.x,
                    "y": pos.y,
                    "terrain": prop.terrain.as_str(),
                    "owner": prop.owner_id.map(|p| p.0)
                }));
            }

            let mut units = vec![];
            let mut unit_query = world.query::<(Entity, &GridPosition, &Faction, &UnitStats, &Health)>();
            for (_entity, pos, faction, stats, health) in unit_query.iter(world) {
                units.push(serde_json::json!({
                    "x": pos.x,
                    "y": pos.y,
                    "player_id": faction.0.0,
                    "unit_type": stats.unit_type.as_str(),
                    "hp": health.current
                }));
            }

            Ok(serde_json::json!({
                "properties": properties,
                "units": units
            }).to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Simulates an AI turn using the AI engine logic.")]
    async fn simulate_ai_turn(&self, #[tool(aggr)] _args: Parameters<SimulateAiTurnArgs>) -> Result<String, String> {
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

    #[tool(description = "Advances to the next game phase/player.")]
    async fn next_phase(&self, #[tool(aggr)] _args: Parameters<NextPhaseArgs>) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            world.send_event(engine::events::NextPhaseCommand);
            state.schedule.run(world);
            
            let ms = world.get_resource::<MatchState>().unwrap();
            let players = world.get_resource::<Players>().unwrap();
            let active_player = &players.0[ms.active_player_index.0];
            
            Ok(format!("Advanced to turn {}, player {} (Phase: {:?})", 
                ms.current_turn_number.0, active_player.id.0, ms.current_phase))
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Executes an action.")]
    async fn execute_action(&self, #[tool(aggr)] _args: Parameters<ExecuteActionArgs>) -> Result<String, String> {
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
