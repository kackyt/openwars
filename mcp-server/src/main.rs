use bevy_ecs::prelude::Entity;
use bevy_ecs::schedule::Schedule;
use bevy_ecs::world::World;
#[allow(dead_code)]
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ServerInfo;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
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
    Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::{MatchState, Players};
use engine::setup::initialize_world_from_master_data;

#[derive(Clone)]
#[allow(dead_code)]
struct OpenWarsAiServer {
    pub state: Arc<Mutex<Option<GameState>>>,
}

fn parse_player_id(value: u64) -> Result<PlayerId, String> {
    let id = u32::try_from(value).map_err(|_| format!("Player ID {} is out of range", value))?;
    Ok(PlayerId(id))
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
    pub player_id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct EvaluateBoardArgs {
    pub player_id: u64,
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
pub struct ExecuteActionArgs {
    pub action_type: String,
    pub unit_id: Option<u64>,
    pub target_id: Option<u64>,
    pub target_x: Option<u32>,
    pub target_y: Option<u32>,
    pub unit_name: Option<String>,
}

#[tool_router]
impl OpenWarsAiServer {
    #[tool(description = "Loads a specific map to evaluate.")]
    async fn load_map(&self, Parameters(args): Parameters<LoadMapArgs>) -> Result<String, String> {
        let registry =
            MasterDataRegistry::load().map_err(|e| format!("Failed to load master data: {}", e))?;

        let (world, schedule) = initialize_world_from_master_data(&registry, &args.map_name)
            .map_err(|e| format!("Initialization failed: {}", e))?;

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState { world, schedule });

        Ok(format!("Loaded map: {}", args.map_name))
    }

    #[tool(description = "Evaluates the board.")]
    async fn evaluate_board(
        &self,
        Parameters(args): Parameters<EvaluateBoardArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let player_id = parse_player_id(args.player_id)?;
            let score = engine::ai::eval::evaluate_board(&mut state.world, player_id);
            Ok(serde_json::json!({
                "player_id": args.player_id,
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
        Parameters(args): Parameters<GetValidActionsArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let entity = Entity::from_bits(args.unit_id);

            if world.get_entity(entity).is_ok() {
                let pos = if let (Some(x), Some(y)) = (args.x, args.y) {
                    engine::components::GridPosition { x, y }
                } else {
                    world
                        .get::<GridPosition>(entity)
                        .cloned()
                        .unwrap_or(GridPosition { x: 0, y: 0 })
                };

                let is_moved = world.get::<HasMoved>(entity).map(|h| h.0).unwrap_or(false);
                let actions =
                    engine::systems::action::get_available_actions_at(world, entity, pos, is_moved);
                Ok(serde_json::to_string(&actions).map_err(|e| e.to_string())?)
            } else {
                Err(format!("Unit with ID {} not found", args.unit_id))
            }
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns reachable tiles for a unit.")]
    async fn get_reachable_tiles(
        &self,
        Parameters(args): Parameters<GetReachableTilesArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;
            let entity = Entity::from_bits(args.unit_id);

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
                        Option<&engine::components::Transporting>,
                    )>();
                    for (e, p, f, s, cargo_opt, transporting_opt) in q_occupants.iter(world) {
                        if e != entity && transporting_opt.is_none() {
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
                    Err(format!("Unit with ID {} is missing stats", args.unit_id))
                }
            } else {
                Err(format!("Unit with ID {} not found", args.unit_id))
            }
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Returns the current state of the board.")]
    async fn get_board_state(
        &self,
        Parameters(_args): Parameters<GetBoardStateArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;

            let mut properties = vec![];
            let mut prop_query = world.query::<(Entity, &GridPosition, &Property)>();
            for (entity, pos, prop) in prop_query.iter(world) {
                properties.push(serde_json::json!({
                    "entity_id": entity.to_bits(),
                    "x": pos.x,
                    "y": pos.y,
                    "terrain": prop.terrain.as_str(),
                    "owner": prop.owner_id.map(|p| p.0 as u64)
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
                    "player_id": p.id.0 as u64,
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
        Parameters(_args): Parameters<SimulateAiTurnArgs>,
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

            let mut actions_taken = vec![];
            loop {
                let action_taken =
                    engine::ai::engine::execute_ai_turn(&mut state.world, active_player_id);

                // イベント処理
                state.schedule.run(&mut state.world);

                if let Some(action) = action_taken {
                    actions_taken.push(action);
                } else {
                    break;
                }
            }

            let after_score = engine::ai::eval::evaluate_board(&mut state.world, active_player_id);

            Ok(serde_json::json!({
                "actions_taken": actions_taken,
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

    #[tool(description = "Executes an action.")]
    async fn execute_action(
        &self,
        Parameters(args): Parameters<ExecuteActionArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let world = &mut state.world;

            match args.action_type.as_str() {
                "next_phase" => {
                    world.send_event(engine::events::NextPhaseCommand);
                }
                "move" => {
                    let unit_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for move".to_string())?,
                    );
                    let target_x = args
                        .target_x
                        .ok_or_else(|| "target_x is required for move".to_string())?
                        as usize;
                    let target_y = args
                        .target_y
                        .ok_or_else(|| "target_y is required for move".to_string())?
                        as usize;
                    world.send_event(engine::events::MoveUnitCommand {
                        unit_entity,
                        target_x,
                        target_y,
                    });
                }
                "attack" => {
                    let attacker_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for attack".to_string())?,
                    );
                    let defender_entity = Entity::from_bits(
                        args.target_id
                            .ok_or_else(|| "target_id is required for attack".to_string())?,
                    );
                    world.send_event(engine::events::AttackUnitCommand {
                        attacker_entity,
                        defender_entity,
                    });
                }
                "capture" => {
                    let unit_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for capture".to_string())?,
                    );
                    world.send_event(engine::events::CapturePropertyCommand { unit_entity });
                }
                "wait" => {
                    let unit_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for wait".to_string())?,
                    );
                    world.send_event(engine::events::WaitUnitCommand { unit_entity });
                }
                "supply" => {
                    let supplier_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for supply".to_string())?,
                    );
                    let target_entity = Entity::from_bits(
                        args.target_id
                            .ok_or_else(|| "target_id is required for supply".to_string())?,
                    );
                    world.send_event(engine::events::SupplyUnitCommand {
                        supplier_entity,
                        target_entity,
                    });
                }
                "load" => {
                    let unit_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id is required for load".to_string())?,
                    );
                    let transport_entity = Entity::from_bits(
                        args.target_id
                            .ok_or_else(|| "target_id is required for load".to_string())?,
                    );
                    world.send_event(engine::events::LoadUnitCommand {
                        transport_entity,
                        unit_entity,
                    });
                }
                "unload" => {
                    let transport_entity =
                        Entity::from_bits(args.unit_id.ok_or_else(|| {
                            "unit_id (transport) is required for unload".to_string()
                        })?);
                    let cargo_entity =
                        Entity::from_bits(args.target_id.ok_or_else(|| {
                            "target_id (cargo) is required for unload".to_string()
                        })?);
                    let target_x = args
                        .target_x
                        .ok_or_else(|| "target_x is required for unload".to_string())?
                        as usize;
                    let target_y = args
                        .target_y
                        .ok_or_else(|| "target_y is required for unload".to_string())?
                        as usize;
                    world.send_event(engine::events::UnloadUnitCommand {
                        transport_entity,
                        cargo_entity,
                        target_x,
                        target_y,
                    });
                }
                "merge" => {
                    let source_entity = Entity::from_bits(
                        args.unit_id
                            .ok_or_else(|| "unit_id (source) is required for merge".to_string())?,
                    );
                    let target_entity = Entity::from_bits(
                        args.target_id
                            .ok_or_else(|| "target_id is required for merge".to_string())?,
                    );
                    world.send_event(engine::events::MergeUnitCommand {
                        source_entity,
                        target_entity,
                    });
                }
                "produce" => {
                    let target_x = args
                        .target_x
                        .ok_or_else(|| "target_x is required for produce".to_string())?
                        as usize;
                    let target_y = args
                        .target_y
                        .ok_or_else(|| "target_y is required for produce".to_string())?
                        as usize;
                    let unit_name_str = args
                        .unit_name
                        .as_ref()
                        .ok_or_else(|| "unit_name is required for produce".to_string())?;
                    let unit_type = engine::resources::UnitType::from_str(unit_name_str)
                        .ok_or_else(|| format!("Unknown unit type: {}", unit_name_str))?;

                    let active_player_id = {
                        let ms = world.resource::<MatchState>();
                        let players = world.resource::<Players>();
                        players
                            .0
                            .get(ms.active_player_index.0)
                            .ok_or_else(|| "Active player index is out of range".to_string())?
                            .id
                    };

                    world.send_event(engine::events::ProduceUnitCommand {
                        player_id: active_player_id,
                        target_x,
                        target_y,
                        unit_type,
                    });
                }
                _ => return Err(format!("Unknown action type: {}", args.action_type)),
            }

            state.schedule.run(world);
            Ok(format!("Executed action: {}", args.action_type))
        } else {
            Err("Map not loaded".into())
        }
    }
}

#[tool_handler(name = "openwars-mcp", version = "1.0.0")]
impl ServerHandler for OpenWarsAiServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
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
