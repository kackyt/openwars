#[cfg_attr(test, allow(dead_code))]
mod invasion_trace;

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
    pub invasion_trace: invasion_trace::InvasionTraceCollector,
}

use engine::components::{
    CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property, Transporting,
    UnitStats,
};
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::{GridTopology, MatchState, Players};
use engine::setup::initialize_world_from_master_data_with_topology;

#[derive(Clone)]
#[allow(dead_code)]
struct OpenWarsAiServer {
    pub state: Arc<Mutex<Option<GameState>>>,
}

// テストプロファイルの bin ビルドでは実 main が置換され、本関数を呼ぶ #[tool] メソッドが
// 到達不能扱いになり dead_code 警告が出る。実バイナリでは使用されているため test 時のみ許可する。
#[cfg_attr(test, allow(dead_code))]
fn parse_player_id(value: u64) -> Result<PlayerId, String> {
    let id = u32::try_from(value).map_err(|_| format!("Player ID {} is out of range", value))?;
    Ok(PlayerId(id))
}

#[derive(Deserialize, JsonSchema)]
pub struct LoadMapArgs {
    pub map_name: String,
    pub seed: Option<u64>,
    pub grid_type: Option<String>,
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
pub struct SetPlayerAiVersionArgs {
    pub player_id: u64,
    pub version: String,
}

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

        let topology = match args
            .grid_type
            .as_deref()
            .unwrap_or("hex")
            .to_lowercase()
            .as_str()
        {
            "square" => GridTopology::Square,
            "hex" => GridTopology::Hex,
            other => {
                return Err(format!(
                    "Unknown grid_type: {}. Expected 'square' or 'hex'",
                    other
                ));
            }
        };

        let (mut world, schedule) =
            initialize_world_from_master_data_with_topology(&registry, &args.map_name, topology)
                .map_err(|e| format!("Initialization failed: {}", e))?;
        if let Some(seed) = args.seed {
            world.insert_resource(engine::resources::GameRng::new(seed));
        }
        let invasion_trace = invasion_trace::InvasionTraceCollector::new(&world);

        let mut state_lock = self.state.lock().await;
        *state_lock = Some(GameState {
            world,
            schedule,
            invasion_trace,
        });

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
            // 主観評価値 (AI バージョン依存の探索スコアと内訳)
            let subjective =
                engine::ai::eval::evaluate_board_with_metrics(&mut state.world, player_id, None);
            // 客観メトリクス (バージョン非依存。V1/V2 比較・検証用)
            let objective =
                engine::ai::eval::compute_objective_metrics(&mut state.world, player_id);
            Ok(serde_json::json!({
                "player_id": args.player_id,
                "score": subjective.total_score,
                "subjective_metrics": subjective,
                "objective_metrics": objective
            })
            .to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    /// プレイヤーのAIバージョンに依存しない客観的な盤面評価メトリクスを計算して返します。
    #[tool(description = "Computes AI-version-independent objective board metrics for a player.")]
    async fn evaluate_board_objective(
        &self,
        Parameters(args): Parameters<EvaluateBoardArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let player_id = parse_player_id(args.player_id)?;
            let objective =
                engine::ai::eval::compute_objective_metrics(&mut state.world, player_id);
            Ok(serde_json::json!({
                "player_id": args.player_id,
                "objective_metrics": objective
            })
            .to_string())
        } else {
            Err("Map not loaded".into())
        }
    }

    #[tool(description = "Sets the AI version for a specific player.")]
    async fn set_player_ai_version(
        &self,
        Parameters(args): Parameters<SetPlayerAiVersionArgs>,
    ) -> Result<String, String> {
        let mut state_lock = self.state.lock().await;
        if let Some(state) = state_lock.as_mut() {
            let player_id = parse_player_id(args.player_id)?;
            let version = match args.version.as_str() {
                "V1" => engine::ai::AiVersion::V1,
                "V2" => engine::ai::AiVersion::V2,
                "V3" => engine::ai::AiVersion::V3,
                "V4" => engine::ai::AiVersion::V4,
                _ => {
                    return Err(format!(
                        "Invalid AI version: {}. Must be 'V1', 'V2', 'V3' or 'V4'",
                        args.version
                    ));
                }
            };

            let mut settings = state
                .world
                .get_resource_mut::<engine::ai::PlayerAiSettings>();
            if let Some(ref mut s) = settings {
                s.set_version(player_id, version);
            } else {
                let mut s = engine::ai::PlayerAiSettings::new();
                s.set_version(player_id, version);
                state.world.insert_resource(s);
            }

            Ok(format!(
                "Successfully set Player {} to {:?}",
                args.player_id, version
            ))
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
            let mut prop_query = world.query::<(Entity, &GridPosition, &Property)>();
            let mut unit_query = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                &Health,
                Option<&Transporting>,
                Option<&CargoCapacity>,
            )>();
            let island_map = world.resource::<engine::ai::islands::IslandMap>();

            let mut properties = vec![];
            for (entity, pos, prop) in prop_query.iter(world) {
                properties.push(serde_json::json!({
                    "entity_id": entity.to_bits(),
                    "x": pos.x,
                    "y": pos.y,
                    "terrain": prop.terrain.as_str(),
                    "terrain_type": format!("{:?}", prop.terrain),
                    "owner": prop.owner_id.map(|p| p.0 as u64),
                    "capture_points": prop.capture_points,
                    "max_capture_points": prop.max_capture_points,
                    "island_id": island_map.get_island_at(pos).map(|island| island.id.0)
                }));
            }

            let mut units = vec![];
            for (entity, pos, faction, stats, health, transporting, cargo) in unit_query.iter(world)
            {
                let mut cargo_ids: Vec<_> = cargo
                    .map(|capacity| {
                        capacity
                            .loaded
                            .iter()
                            .map(|cargo| cargo.to_bits())
                            .collect()
                    })
                    .unwrap_or_default();
                cargo_ids.sort_unstable();
                units.push(serde_json::json!({
                    "unit_id": entity.to_bits(),
                    "x": pos.x,
                    "y": pos.y,
                    "player_id": faction.0.0,
                    "unit_type": stats.unit_type.as_str(),
                    "hp": health.current,
                    "cost": stats.cost,
                    "can_capture": stats.can_capture,
                    "max_cargo": stats.max_cargo,
                    "loadable_unit_types": stats
                        .loadable_unit_types
                        .iter()
                        .map(|unit_type| unit_type.as_str())
                        .collect::<Vec<_>>(),
                    "island_id": island_map.get_island_at(pos).map(|island| island.id.0),
                    "transporting_by": transporting.map(|transporting| transporting.0.to_bits()),
                    "cargo_ids": cargo_ids
                }));
            }

            let players = world.resource::<engine::resources::Players>();
            let match_state = world.resource::<engine::resources::MatchState>();

            // 所有拠点数と生存ユニット総コストの計算
            let mut player_properties_count = std::collections::HashMap::new();
            let mut player_units_cost = std::collections::HashMap::new();

            for (_, _, prop) in prop_query.iter(world) {
                if let Some(owner) = prop.owner_id {
                    *player_properties_count.entry(owner.0).or_insert(0) += 1;
                }
            }

            for (_, _, faction, stats, health, _, _) in unit_query.iter(world) {
                if health.current > 0 {
                    *player_units_cost.entry(faction.0.0).or_insert(0) += stats.cost;
                }
            }

            let game_over_info = match &match_state.game_over {
                Some(engine::resources::GameOverCondition::Winner(pid)) => serde_json::json!({
                    "status": "winner",
                    "winner_id": pid.0
                }),
                Some(engine::resources::GameOverCondition::Draw) => serde_json::json!({
                    "status": "draw"
                }),
                None => serde_json::json!(null),
            };

            let mut players_info = vec![];
            for p in &players.0 {
                let pid_u32 = p.id.0;
                let prop_count = player_properties_count.get(&pid_u32).copied().unwrap_or(0);
                let unit_cost = player_units_cost.get(&pid_u32).copied().unwrap_or(0);

                players_info.push(serde_json::json!({
                    "player_id": p.id.0 as u64,
                    "name": p.name,
                    "funds": p.funds,
                    "property_count": prop_count,
                    "unit_cost": unit_cost
                }));
            }

            let transport_squads = invasion_trace::snapshot_transport_squads(world);
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

            Ok(serde_json::json!({
                "turn": match_state.current_turn_number.0,
                "active_player_index": match_state.active_player_index.0,
                "phase": format!("{:?}", match_state.current_phase),
                "game_over": game_over_info,
                "players": players_info,
                "properties": properties,
                "units": units,
                "transport_squads": transport_squads,
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

            let before_metrics = engine::ai::eval::evaluate_board_with_metrics(
                &mut state.world,
                active_player_id,
                None,
            );

            let mut actions_taken = vec![];
            let mut invasion_events = vec![];
            let mut factory_relief = vec![];
            let mut step = 0usize;
            loop {
                let turn = state.world.resource::<MatchState>().current_turn_number.0;
                let units_before = invasion_trace::snapshot_units(&mut state.world);
                let action_taken =
                    engine::ai::engine::execute_ai_turn(&mut state.world, active_player_id);
                // 解除攻撃で対象が消える前に、この手番中に一度でも生成された計画を保持する。
                for mission in
                    invasion_trace::snapshot_factory_relief_plan(&state.world, active_player_id)
                {
                    if !factory_relief.iter().any(
                        |existing: &invasion_trace::FactoryReliefMissionSnapshot| {
                            existing.assigned_entity == mission.assigned_entity
                                && existing.threat_entity == mission.threat_entity
                        },
                    ) {
                        factory_relief.push(mission);
                    }
                }

                // イベント処理後に、実行済みの侵攻イベントだけを構造化して収集する。
                state.schedule.run(&mut state.world);
                invasion_events.extend(state.invasion_trace.collect_step(
                    &mut state.world,
                    turn,
                    active_player_id.0,
                    step,
                    &units_before,
                ));
                step += 1;

                if let Some(action) = action_taken {
                    actions_taken.push(action);
                } else {
                    break;
                }
            }
            let transport_squads = invasion_trace::snapshot_transport_squads(&state.world);
            // V3の直近分析が存在する場合だけ島別診断を返し、V1ではnullを維持する。
            let island_campaign =
                invasion_trace::snapshot_island_campaign_for_player(&state.world, active_player_id);
            // 遊兵（任務なし・任務があるのに動けない）の計測結果。engine側がターン終了直前に記録する。
            let idle_audit =
                invasion_trace::snapshot_idle_audit_for_player(&state.world, active_player_id);
            // V4の生産判断内訳。V1〜V3は記録が無いためnullになる。
            let production_plan =
                invasion_trace::snapshot_production_plan_for_player(&state.world, active_player_id);
            // 生産意図から実Entityの任務・攻撃まで接続された実績。V4以外は空になる。
            let deployment_audit = invasion_trace::snapshot_deployment_audit_for_player(
                &state.world,
                active_player_id,
            );
            let plan_revisions =
                invasion_trace::snapshot_plan_revisions_for_player(&state.world, active_player_id);
            let plan_executions =
                invasion_trace::snapshot_plan_executions_for_player(&state.world, active_player_id);
            // 勝利条件から島作戦・担当Entity・実行Eventまでを同じIDで追跡する。
            let victory_roadmap =
                invasion_trace::snapshot_victory_roadmap_for_player(&state.world, active_player_id);
            let logistics_plan =
                invasion_trace::snapshot_logistics_plan_for_player(&state.world, active_player_id);
            let emergency_plan =
                invasion_trace::snapshot_emergency_plan_for_player(&state.world, active_player_id);
            let after_metrics = engine::ai::eval::evaluate_board_with_metrics(
                &mut state.world,
                active_player_id,
                None,
            );

            Ok(serde_json::json!({
                "actions_taken": actions_taken,
                "invasion_events": invasion_events,
                "transport_squads": transport_squads,
                "island_campaign": island_campaign,
                "idle_audit": idle_audit,
                "production_plan": production_plan,
                "deployment_audit": deployment_audit,
                "plan_revisions": plan_revisions,
                "plan_executions": plan_executions,
                "victory_roadmap": victory_roadmap,
                "logistics_plan": logistics_plan,
                "emergency_plan": emergency_plan,
                "factory_relief": factory_relief,
                "player_id": active_player_id.0,
                "player_index": active_player_index.0,
                "before_score": before_metrics.total_score,
                "after_score": after_metrics.total_score,
                "before_metrics": before_metrics,
                "after_metrics": after_metrics
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

// AI思考は HashMap の反復順に依存する箇所が残っており、
// マルチスレッドランタイムだとツール呼び出しごとに別ワーカースレッドへ載る。
// HashMap の RandomState はスレッドローカルな種を使うため、
// ターンごとに生成されるマップの反復順が変わり、同一seedでも結果が再現しなくなる。
// ターン制ゲームの進行は本質的に逐次処理なので、単一スレッドランタイムに固定して再現性を確保する。
#[tokio::main(flavor = "current_thread")]
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
