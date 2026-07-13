use wasm_bindgen::prelude::*;
use bevy_ecs::prelude::*;

use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MatchState, Phase, Terrain, Players};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, Property,
    UnitStats,
};

#[wasm_bindgen]
pub struct WasmEngine {
    world: World,
    schedule: Schedule,
}

#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(map_name: &str, topology_str: &str) -> Result<WasmEngine, JsValue> {
        let master_data = MasterDataRegistry::load()
            .map_err(|e| JsValue::from_str(&format!("Failed to load master data: {:?}", e)))?;
        
        let topology = match topology_str {
            "hex" => crate::resources::GridTopology::Hex,
            _ => crate::resources::GridTopology::Square,
        };
        
        let (world, schedule) = crate::setup::initialize_world_from_master_data_with_topology(&master_data, map_name, topology)
            .map_err(|e| JsValue::from_str(&format!("Failed to init world: {:?}", e)))?;
            
        Ok(WasmEngine { world, schedule })
    }

    pub fn get_turn_info(&self) -> JsValue {
        let mut turn = 1;
        let mut phase_str = "P1".to_string();
        let mut funds = 0;
        let mut active_player_index = 0;
        
        if let Some(match_state) = self.world.get_resource::<MatchState>() {
            turn = match_state.current_turn_number.0;
            active_player_index = match_state.active_player_index.0;
            phase_str = match match_state.current_phase {
                Phase::Main => format!("P{}", active_player_index + 1),
                _ => format!("{:?}", match_state.current_phase),
            };
        }
        
        if let Some(players) = self.world.get_resource::<crate::resources::Players>() {
            if let Some(p) = players.0.get(active_player_index) {
                funds = p.funds;
            }
        }
        
        let json = format!(r#"{{"turn": {}, "phase": "{}", "funds": {}}}"#, turn, phase_str, funds);
        JsValue::from_str(&json)
    }

    pub fn get_map(&self) -> JsValue {
        if let Some(map) = self.world.get_resource::<Map>() {
            let width = map.width;
            let height = map.height;
            
            let mut rows = Vec::new();
            for y in 0..height {
                let mut row = Vec::new();
                for x in 0..width {
                    let terrain = map.get_terrain(x, y).unwrap_or(Terrain::Plains);
                    let terrain_name = format!("{:?}", terrain).to_lowercase();
                    row.push(format!("\"{}\"", terrain_name));
                }
                rows.push(format!("[{}]", row.join(",")));
            }
            let json = format!("[{}]", rows.join(",\n"));
            return JsValue::from_str(&json);
        }
        JsValue::from_str("[]")
    }

    pub fn get_units(&mut self) -> JsValue {
        let mut units = Vec::new();
        let mut query = self.world.query::<(Entity, &GridPosition, &Faction, &UnitStats, Option<&Health>, Option<&crate::components::Transporting>, Option<&crate::components::CargoCapacity>)>();
        
        for (entity, pos, faction, stats, hp_opt, trans_opt, cargo_opt) in query.iter(&self.world) {
            if trans_opt.is_some() {
                continue; // 搭載されているユニットは除外
            }
            
            let faction_str = match faction.0.0 {
                1 => "green", // Map P1 to green
                2 => "blue",  // Map P2 to blue
                _ => "unknown",
            };
            let unit_type_str = format!("{:?}", stats.unit_type).to_lowercase();
            let hp = if let Some(h) = hp_opt { (h.current.saturating_add(9)) / 10 } else { 10 };
            let is_loaded = cargo_opt.map_or(false, |c| !c.loaded.is_empty());
            
            let unit_json = format!(
                r#"{{"id": "{}", "type": "{}", "faction": "{}", "x": {}, "y": {}, "hp": {}, "is_loaded": {}}}"#,
                entity.to_bits(), unit_type_str, faction_str, pos.x, pos.y, hp, is_loaded
            );
            units.push(unit_json);
        }
        
        let json = format!("[{}]", units.join(","));
        JsValue::from_str(&json)
    }

    pub fn get_properties(&mut self) -> JsValue {
        let mut properties = Vec::new();
        let mut query = self.world.query::<(&GridPosition, &crate::components::Property)>();
        
        for (pos, property) in query.iter(&self.world) {
            let owner_str = match property.owner_id {
                Some(crate::components::PlayerId(1)) => "green",
                Some(crate::components::PlayerId(2)) => "blue",
                _ => "neutral",
            };
            let terrain_str = format!("{:?}", property.terrain).to_lowercase();
            
            let json = format!(
                r#"{{"x": {}, "y": {}, "type": "{}", "owner": "{}", "capture_points": {}, "max_capture_points": {}}}"#,
                pos.x, pos.y, terrain_str, owner_str, property.capture_points, property.max_capture_points
            );
            properties.push(json);
        }
        
        let json = format!("[{}]", properties.join(","));
        JsValue::from_str(&json)
    }

    pub fn get_terrain_defs(&self) -> JsValue {
        if let Some(master) = self.world.get_resource::<crate::resources::MasterDataRegistry>() {
            let mut defs = Vec::new();
            for &(terrain, _) in crate::resources::TERRAIN_MAP {
                let def = master.get_terrain_defense_bonus(terrain);
                let terrain_str = format!("{:?}", terrain).to_lowercase();
                defs.push(format!(r#""{}": {}"#, terrain_str, def));
            }
            let json = format!("{{{}}}", defs.join(","));
            JsValue::from_str(&json)
        } else {
            JsValue::from_str("{}")
        }
    }

    /// AIの1アクションを実行する。行動があればtrue、なければfalseを返す。
    pub fn execute_ai_turn(&mut self) -> bool {
        // PlayerIndex（0, 1）ではなく、Playersリソースから実際のPlayerId（1, 2）を取得する
        let active_player = {
            let idx = self.world.get_resource::<MatchState>()
                .map(|ms| ms.active_player_index.0);
            let Some(idx) = idx else { return false };
            let pid = self.world.get_resource::<crate::resources::Players>()
                .and_then(|players| players.0.get(idx).map(|p| p.id));
            let Some(pid) = pid else { return false };
            pid
        };

        let res = crate::ai::engine::execute_ai_turn(&mut self.world, active_player);
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        res.is_some()
    }

    pub fn submit_end_turn_command(&mut self) -> JsValue {
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>() {
            evs.send(crate::events::NextPhaseCommand);
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn check_game_over(&self) -> JsValue {
        if let Some(match_state) = self.world.get_resource::<MatchState>() {
            if let Some(game_over) = &match_state.game_over {
                match game_over {
                    crate::resources::GameOverCondition::Winner(player_id) => {
                        return JsValue::from_str(&format!(r#"{{"winner": {}}}"#, player_id.0));
                    },
                    crate::resources::GameOverCondition::Draw => {
                        return JsValue::from_str(r#"{"draw": true}"#);
                    }
                }
            }
        }
        JsValue::from_str("null")
    }

    pub fn get_reachable_cells(&mut self, unit_id_str: &str) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let target_entity = Entity::from_bits(unit_entity_bits);
        
        let mut start_pos = None;
        let mut mov_type = None;
        let mut max_mov = 0;
        let mut fuel_cur = 0;
        let mut active_player_id = crate::components::PlayerId(1);
        let mut u_type = crate::resources::UnitType::Infantry;

        if let Some(pos) = self.world.get::<GridPosition>(target_entity) {
            start_pos = Some((pos.x, pos.y));
        }
        if let Some(stats) = self.world.get::<UnitStats>(target_entity) {
            mov_type = Some(stats.movement_type);
            max_mov = stats.max_movement;
            u_type = stats.unit_type;
        }
        if let Some(fuel) = self.world.get::<crate::components::Fuel>(target_entity) {
            fuel_cur = fuel.current;
        }
        if let Some(faction) = self.world.get::<Faction>(target_entity) {
            active_player_id = faction.0;
        }

        let mut unit_positions = std::collections::HashMap::new();
        let mut q_all = self.world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (e, p, f, s, c, t) in q_all.iter(&self.world) {
            if e == target_entity || t.is_some() {
                continue;
            }
            let free_slots = c.map(|c| c.max.saturating_sub(c.loaded.len() as u32)).unwrap_or(0);
            unit_positions.insert(
                (p.x, p.y),
                crate::systems::movement::OccupantInfo {
                    player_id: f.0,
                    is_transport: s.max_cargo > 0,
                    unit_type: s.unit_type,
                    loadable_types: s.loadable_unit_types.clone(),
                    free_slots,
                },
            );
        }

        if let (Some(start), Some(m_type)) = (start_pos, mov_type) {
            if let (Some(map), Some(master_data)) = (
                self.world.get_resource::<Map>(),
                self.world.get_resource::<crate::resources::master_data::MasterDataRegistry>(),
            ) {
                let reachable = crate::systems::movement::calculate_reachable_tiles(
                    map,
                    &unit_positions,
                    start,
                    m_type,
                    max_mov,
                    fuel_cur,
                    active_player_id,
                    u_type,
                    master_data,
                );
                
                let mut coords = Vec::new();
                for &(rx, ry) in &reachable {
                    coords.push(format!(r#"{{"x": {}, "y": {}}}"#, rx, ry));
                }
                let json = format!("[{}]", coords.join(","));
                return JsValue::from_str(&json);
            }
        }
        JsValue::from_str("[]")
    }

    pub fn get_available_actions(&mut self, unit_id_str: &str, dest_x: i32, dest_y: i32) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        
        let mut is_moved = false;
        if let Some(pos) = self.world.get::<GridPosition>(unit_entity) {
            if pos.x != dest_x as usize || pos.y != dest_y as usize {
                is_moved = true;
            }
        }
        
        let actions = crate::systems::action::get_available_actions_at(
            &mut self.world, 
            unit_entity, 
            crate::components::GridPosition { x: dest_x as usize, y: dest_y as usize },
            is_moved
        );
        
        let mut options = Vec::new();
        if actions.can_wait { options.push("\"Wait\""); }
        if actions.can_attack { options.push("\"Attack\""); }
        if actions.can_capture { options.push("\"Capture\""); }
        if actions.can_supply { options.push("\"Supply\""); }
        if actions.can_merge { options.push("\"Merge\""); }
        if actions.can_load { options.push("\"Load\""); }
        if actions.can_drop { options.push("\"Drop\""); }
        
        let json = format!("[{}]", options.join(","));
        JsValue::from_str(&json)
    }

    pub fn get_producible_units(&mut self, x: i32, y: i32) -> JsValue {
        let active_player_id = if let Some(match_state) = self.world.get_resource::<MatchState>() {
            if let Some(players) = self.world.get_resource::<Players>() {
                players.0.get(match_state.active_player_index.0).map(|p| p.id).unwrap_or(crate::components::PlayerId(0))
            } else { crate::components::PlayerId(0) }
        } else { crate::components::PlayerId(0) };

        let master_data_opt = self.world.get_resource::<crate::resources::master_data::MasterDataRegistry>().cloned();

        let mut producible = Vec::new();
        if let Some(master_data) = master_data_opt {
            if crate::systems::production::can_produce_at_tile(&mut self.world, active_player_id, x as usize, y as usize, &master_data).is_ok() {
                let mut target_prop = None;
                let mut q_prop = self.world.query::<(&GridPosition, &Property)>();
                for (pos, prop) in q_prop.iter(&self.world) {
                    if pos.x == x as usize && pos.y == y as usize {
                        target_prop = Some(prop.clone());
                        break;
                    }
                }
                
                if let Some(prop) = target_prop {
                    for (name, record) in &master_data.units {
                        if let Ok(u_type) = master_data.unit_type_for_name(&name.0) {
                            if master_data.can_produce_unit(prop.terrain.as_str(), u_type) {
                                producible.push(format!(
                                    r#"{{"type": "{}", "name": "{}", "cost": {}}}"#,
                                    format!("{:?}", u_type).to_lowercase(), record.name.0, record.cost
                                ));
                            }
                        }
                    }
                }
            }
        }
        
        let json = format!("[{}]", producible.join(","));
        JsValue::from_str(&json)
    }

    pub fn get_attackable_targets(&mut self, unit_id_str: &str, dest_x: i32, dest_y: i32) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        
        let mut is_moved = false;
        if let Some(pos) = self.world.get::<GridPosition>(unit_entity) {
            if pos.x != dest_x as usize || pos.y != dest_y as usize {
                is_moved = true;
            }
        }
        
        let dest_pos = GridPosition { x: dest_x as usize, y: dest_y as usize };
        let targets = crate::systems::combat::get_attackable_targets_at(&mut self.world, unit_entity, dest_pos, !is_moved);
        
        let mut target_list = Vec::new();
        for target_entity in targets {
            if let Some(pos) = self.world.get::<GridPosition>(target_entity) {
                target_list.push(format!(
                    r#"{{"id": "{}", "x": {}, "y": {}}}"#,
                    target_entity.to_bits(), pos.x, pos.y
                ));
            }
        }
        
        let json = format!("[{}]", target_list.join(","));
        JsValue::from_str(&json)
    }

    pub fn submit_move_command(&mut self, unit_id_str: &str, x: i32, y: i32) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::MoveUnitCommand>>() {
            evs.send(crate::events::MoveUnitCommand {
                unit_entity,
                target_x: x as usize,
                target_y: y as usize,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn submit_wait_command(&mut self, unit_id_str: &str) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::WaitUnitCommand>>() {
            evs.send(crate::events::WaitUnitCommand { unit_entity });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn submit_attack_command(&mut self, unit_id_str: &str, target_id_str: &str) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        let target_entity_bits = target_id_str.parse::<u64>().unwrap_or(0);
        let target_entity = Entity::from_bits(target_entity_bits);
        
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::AttackUnitCommand>>() {
            evs.send(crate::events::AttackUnitCommand {
                attacker_entity: unit_entity,
                defender_entity: target_entity,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn submit_capture_command(&mut self, unit_id_str: &str) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::CapturePropertyCommand>>() {
            evs.send(crate::events::CapturePropertyCommand { unit_entity });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn submit_load_command(&mut self, unit_id_str: &str, target_id_str: &str) -> JsValue {
        let unit_entity_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_entity_bits);
        let target_entity_bits = target_id_str.parse::<u64>().unwrap_or(0);
        let transport_entity = Entity::from_bits(target_entity_bits);
        
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::LoadUnitCommand>>() {
            evs.send(crate::events::LoadUnitCommand {
                transport_entity,
                unit_entity,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn submit_produce_command(&mut self, unit_type_str: &str, x: i32, y: i32) -> JsValue {
        let unit_type = crate::resources::UnitType::from_english_str(unit_type_str)
            .unwrap_or(crate::resources::UnitType::Infantry);

        let active_player_id = if let Some(match_state) = self.world.get_resource::<MatchState>() {
            if let Some(players) = self.world.get_resource::<Players>() {
                players.0.get(match_state.active_player_index.0).map(|p| p.id).unwrap_or(crate::components::PlayerId(0))
            } else { crate::components::PlayerId(0) }
        } else { crate::components::PlayerId(0) };

        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>() {
            evs.send(crate::events::ProduceUnitCommand {
                player_id: active_player_id,
                unit_type,
                target_x: x as usize,
                target_y: y as usize,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    pub fn end_turn(&mut self) -> JsValue {
        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>() {
            evs.send(crate::events::NextPhaseCommand);
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        
        // PhaseがEndTurnになった場合は、次のターンの処理へ遷移させる
        let mut needs_advance = false;
        if let Some(match_state) = self.world.get_resource::<MatchState>() {
            if match_state.current_phase == Phase::EndTurn {
                needs_advance = true;
            }
        }
        if needs_advance {
            crate::systems::turn_management::advance_next_phase(&mut self.world);
        }

        JsValue::from_str("{}")
    }

    /// 指定された輸送ユニットに積載されているユニットの一覧を返します。
    /// 降車メニューの選択肢を表示するために使用します。
    pub fn get_loaded_units(&mut self, transport_id_str: &str) -> JsValue {
        let transport_bits = transport_id_str.parse::<u64>().unwrap_or(0);
        let transport_entity = Entity::from_bits(transport_bits);

        // まず積載ユニットのエンティティIDリストを取得する（借用の分離）
        let passengers: Vec<Entity> = {
            let mut q = self.world.query::<&CargoCapacity>();
            q.get(&self.world, transport_entity)
                .map(|c| c.loaded.clone())
                .unwrap_or_default()
        };

        let mut result = Vec::new();
        for passenger in passengers {
            let mut q_stats = self.world.query::<&UnitStats>();
            if let Ok(stats) = q_stats.get(&self.world, passenger) {
                let unit_type_str = format!("{:?}", stats.unit_type).to_lowercase();
                result.push(format!(
                    r#"{{"id": "{}", "type": "{}"}}"#,
                    passenger.to_bits(), unit_type_str
                ));
            }
        }

        let json = format!("[{}]", result.join(","));
        JsValue::from_str(&json)
    }

    /// 指定された輸送ユニットから指定された積載ユニットを降車させることが可能なマス一覧を返します。
    pub fn get_droppable_tiles(&mut self, transport_id_str: &str, cargo_id_str: &str) -> JsValue {
        let transport_bits = transport_id_str.parse::<u64>().unwrap_or(0);
        let transport_entity = Entity::from_bits(transport_bits);
        let cargo_bits = cargo_id_str.parse::<u64>().unwrap_or(0);
        let cargo_entity = Entity::from_bits(cargo_bits);

        let tiles = crate::systems::transport::get_droppable_tiles(
            &mut self.world,
            transport_entity,
            cargo_entity,
        );

        let mut coords = Vec::new();
        for (x, y) in tiles {
            coords.push(format!(r#"{{"x": {}, "y": {}}}"#, x, y));
        }
        let json = format!("[{}]", coords.join(","));
        JsValue::from_str(&json)
    }

    /// 輸送ユニットから指定されたユニットを指定のマスへ降車させるコマンドを送信します。
    pub fn submit_unload_command(
        &mut self,
        transport_id_str: &str,
        cargo_id_str: &str,
        target_x: i32,
        target_y: i32,
    ) -> JsValue {
        let transport_bits = transport_id_str.parse::<u64>().unwrap_or(0);
        let transport_entity = Entity::from_bits(transport_bits);
        let cargo_bits = cargo_id_str.parse::<u64>().unwrap_or(0);
        let cargo_entity = Entity::from_bits(cargo_bits);

        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::UnloadUnitCommand>>() {
            evs.send(crate::events::UnloadUnitCommand {
                transport_entity,
                cargo_entity,
                target_x: target_x as usize,
                target_y: target_y as usize,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }

    /// ユニット同士を合流させるコマンドを送信します。
    /// source ユニットが target ユニットに吸収される形で合流します。
    pub fn submit_merge_command(
        &mut self,
        unit_id_str: &str,
        target_id_str: &str,
    ) -> JsValue {
        let unit_bits = unit_id_str.parse::<u64>().unwrap_or(0);
        let unit_entity = Entity::from_bits(unit_bits);
        let target_bits = target_id_str.parse::<u64>().unwrap_or(0);
        let target_entity = Entity::from_bits(target_bits);

        if let Some(mut evs) = self.world.get_resource_mut::<Events<crate::events::MergeUnitCommand>>() {
            evs.send(crate::events::MergeUnitCommand {
                source_entity: unit_entity,
                target_entity,
            });
        }
        self.schedule.run(&mut self.world);
        crate::setup::update_all_events(&mut self.world);
        JsValue::from_str("{}")
    }
}
