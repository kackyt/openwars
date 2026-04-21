use bevy_ecs::prelude::Entity;
use bevy_ecs::schedule::Schedule;
use bevy_ecs::world::World;
#[allow(dead_code)]
use rmcp::handler::server::tool::Parameters;
use rmcp::model::{Implementation, ServerInfo};
use rmcp::{ServerHandler, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

#[allow(dead_code)]
struct GameState {
    pub world: World,
    pub schedule: Schedule,
}

use engine::components::{
    ActionCompleted, Ammo, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property,
    UnitStats,
};
use engine::resources::master_data::{MasterDataRegistry, UnitName};
use engine::resources::{MatchState, Players};
use engine::setup::initialize_world_from_master_data;

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
    pub unit_id: u64,
    pub x: Option<usize>,
    pub y: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetReachableTilesArgs {
    pub unit_id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct NextPhaseArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct ExecuteActionArgs {
    pub unit_id: u64,
    pub action_type: String,
    pub target_id: Option<u64>,
    pub target_x: Option<u32>,
    pub target_y: Option<u32>,
}

#[tool(tool_box)]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate.")]
    async fn load_map(
        &self,
        #[tool(aggr)] args: Parameters<LoadMapArgs>,
    ) -> Result<String, String> {
        let registry =
            MasterDataRegistry::load().map_err(|e| format!("Failed to load master data: {}", e))?;

        let (world, schedule) = initialize_world_from_master_data(&registry, &args.0.map_name)
            .map_err(|e| format!("Initialization failed: {}", e))?;

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState { world, schedule });

        Ok(format!("Loaded map: {}", args.0.map_name))
    }

    #[tool(description = "Spawns a specific unit at a given coordinate.")]
    async fn spawn_unit(
        &self,
        #[tool(aggr)] args: Parameters<SpawnUnitArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let registry = world.resource::<MasterDataRegistry>().clone();

            let unit_name = UnitName(args.0.unit_name.clone());
            let stats = registry
                .create_unit_stats(&unit_name)
                .map_err(|e| format!("Failed to create unit stats: {}", e))?;

            world.spawn((
                GridPosition {
                    x: args.0.x as usize,
                    y: args.0.y as usize,
                },
                Faction(PlayerId(args.0.player_id)),
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
                Ammo {
                    ammo1: stats.max_ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                HasMoved(false),
                ActionCompleted(false),
            ));

            Ok(format!(
                "Spawned {} at ({}, {}) for player {}",
                args.0.unit_name, args.0.x, args.0.y, args.0.player_id
            ))
        } else {
            Err("No map loaded".into())
        }
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(
        &self,
        #[tool(aggr)] args: Parameters<EvaluateBoardArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let score =
                engine::ai::eval::evaluate_board(&mut state.world, PlayerId(args.0.player_id));
            Ok(serde_json::json!({
                "player_id": args.0.player_id,
                "score": score
            })
            .to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns valid actions for a unit at a given position.")]
    async fn get_valid_actions(
        &self,
        #[tool(aggr)] args: Parameters<GetValidActionsArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let entity = Entity::from_bits(args.0.unit_id as u64);

            if world.get_entity(entity).is_ok() {
                let pos = if let (Some(x), Some(y)) = (args.0.x, args.0.y) {
                    engine::components::GridPosition { x, y }
                } else {
                    world
                        .get::<GridPosition>(entity)
                        .cloned()
                        .unwrap_or(GridPosition { x: 0, y: 0 })
                };

                let is_moved = world.get::<HasMoved>(entity).map(|h| h.0).unwrap_or(false);
                let actions = engine::systems::action::get_available_actions_at(
                    world,
                    entity,
                    pos,
                    is_moved,
                );
                Ok(serde_json::to_string(&actions).map_err(|e| e.to_string())?)
            } else {
                Err(format!("Unit with ID {} not found", args.0.unit_id))
            }
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns reachable tiles for a unit.")]
    async fn get_reachable_tiles(
        &self,
        #[tool(aggr)] args: Parameters<GetReachableTilesArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let entity = Entity::from_bits(args.0.unit_id as u64);

            if let Ok(e) = world.get_entity(entity) {
                if let (Some(pos), Some(faction), Some(stats), Some(fuel)) = (
                    e.get::<GridPosition>().cloned(),
                    e.get::<Faction>().cloned(),
                    e.get::<UnitStats>().cloned(),
                    e.get::<Fuel>().cloned(),
                ) {
                let mut unit_positions = std::collections::HashMap::new();
                let mut q_occupants = world.query::<(
                    Entity,
                    &GridPosition,
                    &Faction,
                    &UnitStats,
                    Option<&engine::components::CargoCapacity>,
                )>();
                for (e, p, f, s, cargo_opt) in q_occupants.iter(world) {
                    if e != entity {
                        let free_slots = cargo_opt
                            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                            .unwrap_or(0);
                        unit_positions.insert(
                            (p.x, p.y),
                            engine::systems::movement::OccupantInfo {
                                player_id: f.0,
                                is_transport: s.max_cargo > 0,
                                unit_type: s.unit_type,
                                loadable_types: s.loadable_unit_types.clone(),
                                free_slots,
                            },
                        );
                    }
                }

                let map = world.resource::<engine::resources::Map>();
                let registry = world.resource::<MasterDataRegistry>();

                let reachable = engine::systems::movement::calculate_reachable_tiles(
                    map,
                    &unit_positions,
                    (pos.x, pos.y),
                    stats.movement_type,
                    stats.max_movement,
                    fuel.current,
                    faction.0,
                    stats.unit_type,
                    registry,
                );

                let tiles: Vec<_> = reachable.into_iter().map(|(x, y)| vec![x, y]).collect();
                Ok(serde_json::to_string(&tiles).map_err(|e| e.to_string())?)
                } else {
                    Err(format!("Unit with ID {} is missing stats", args.0.unit_id))
                }
            } else {
                Err(format!("Unit with ID {} not found", args.0.unit_id))
            }
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns the current state of the board.")]
    async fn get_board_state(
        &self,
        #[tool(aggr)] _args: Parameters<GetBoardStateArgs>,
    ) -> Result<String, String> {
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
            let mut unit_query =
                world.query::<(Entity, &GridPosition, &Faction, &UnitStats, &Health)>();
            for (entity, pos, faction, stats, health) in unit_query.iter(world) {
                units.push(serde_json::json!({
                    "unit_id": entity.to_bits(),
                    "x": pos.x,
                    "y": pos.y,
                    "player_id": faction.0.0,
                    "unit_type": stats.unit_type.as_str(),
                    "hp": health.current
                }));
            }

            let players = world.resource::<engine::resources::Players>();
            let mut players_info = vec![];
            for p in &players.0 {
                players_info.push(serde_json::json!({
                    "player_id": p.id.0,
                    "name": p.name,
                    "funds": p.funds
                }));
            }

            let diagnostic = world.get_resource::<engine::resources::ProductionDiagnostic>();
            let diag_info = if let Some(d) = diagnostic {
                serde_json::json!({
                    "last_error": d.last_error,
                    "last_event": d.last_event,
                    "income_log": d.income_log
                })
            } else {
                serde_json::json!({})
            };

            let match_state = world.resource::<engine::resources::MatchState>();

            Ok(serde_json::json!({
                "turn": match_state.current_turn_number.0,
                "active_player_index": match_state.active_player_index.0,
                "phase": format!("{:?}", match_state.current_phase),
                "players": players_info,
                "properties": properties,
                "units": units,
                "diagnostics": diag_info
            })
            .to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Simulates an AI turn using the AI engine logic.")]
    async fn simulate_ai_turn(
        &self,
        #[tool(aggr)] _args: Parameters<SimulateAiTurnArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let (active_player_id, active_player_index) = {
                let ms = state
                    .world
                    .get_resource::<MatchState>()
                    .ok_or("No MatchState")?;
                let players = state.world.get_resource::<Players>().ok_or("No Players")?;
                let p = players
                    .0
                    .get(ms.active_player_index.0)
                    .ok_or("No active player")?;
                (p.id, ms.active_player_index)
            };

            let before_score = engine::ai::eval::evaluate_board(&mut state.world, active_player_id);
            let action_taken =
                engine::ai::engine::execute_ai_turn(&mut state.world, active_player_id);

            // 重要: AIの行動(Event)を発行したあとは、システムを実行して状態を更新する必要がある
            state.schedule.run(&mut state.world);

            let after_score = engine::ai::eval::evaluate_board(&mut state.world, active_player_id);

            Ok(serde_json::json!({
                "action_taken": action_taken,
                "player_id": active_player_id.0,
                "player_index": active_player_index.0,
                "before_score": before_score,
                "after_score": after_score
            })
            .to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Advances to the next game phase/player.")]
    async fn next_phase(
        &self,
        #[tool(aggr)] _args: Parameters<NextPhaseArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            world.send_event(engine::events::NextPhaseCommand);
            state.schedule.run(world);

            let ms = world.get_resource::<MatchState>().unwrap();
            let players = world.get_resource::<Players>().unwrap();
            let active_player = &players.0[ms.active_player_index.0];

            Ok(format!(
                "Advanced to turn {}, player {} (Phase: {:?})",
                ms.current_turn_number.0, active_player.id.0, ms.current_phase
            ))
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Executes an action.")]
    async fn execute_action(
        &self,
        #[tool(aggr)] _args: Parameters<ExecuteActionArgs>,
    ) -> Result<String, String> {
        Ok("Executed action (Stated stub, please use JSON params for specific commands)".into())
    }
}

impl ServerHandler for OpenWarsAiServer {
    rmcp::tool_box!(@derive);

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
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
