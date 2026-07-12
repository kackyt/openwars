use wasm_bindgen::prelude::*;
use js_sys::Promise;
use wasm_bindgen_futures::future_to_promise;
use bevy_ecs::prelude::*;

use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MatchState, Phase, Terrain};
use crate::components::{GridPosition, Faction, UnitStats, Health};

#[wasm_bindgen]
pub struct WasmEngine {
    world: World,
    schedule: Schedule,
}

#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmEngine, JsValue> {
        let master_data = MasterDataRegistry::load()
            .map_err(|e| JsValue::from_str(&format!("Failed to load master data: {:?}", e)))?;
        
        let (world, schedule) = crate::setup::initialize_world_from_master_data(&master_data, "map_1")
            .map_err(|e| JsValue::from_str(&format!("Failed to init world: {:?}", e)))?;
            
        Ok(WasmEngine { world, schedule })
    }

    pub fn get_turn_info(&self) -> JsValue {
        let mut turn = 1;
        let mut phase_str = "P1".to_string();
        
        if let Some(match_state) = self.world.get_resource::<MatchState>() {
            turn = match_state.current_turn_number.0;
            phase_str = match match_state.current_phase {
                Phase::Main => format!("P{}", match_state.active_player_index.0 + 1),
                _ => format!("{:?}", match_state.current_phase),
            };
        }
        
        let json = format!(r#"{{"turn": {}, "phase": "{}"}}"#, turn, phase_str);
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
        let mut query = self.world.query::<(Entity, &GridPosition, &Faction, &UnitStats, Option<&Health>)>();
        
        for (entity, pos, faction, stats, hp_opt) in query.iter(&self.world) {
            let faction_str = match faction.0.0 {
                1 => "green", // Map P1 to green
                2 => "blue",  // Map P2 to blue
                _ => "unknown",
            };
            let unit_type_str = format!("{:?}", stats.unit_type).to_lowercase();
            // HPの表示は get_display_hp() （(current + 9)/10）を使用する。Damagable Traitは使えないので手動計算。
            let hp = if let Some(h) = hp_opt { (h.current.saturating_add(9)) / 10 } else { 10 };
            
            let unit_json = format!(
                r#"{{"id": "{}", "type": "{}", "faction": "{}", "x": {}, "y": {}, "hp": {}}}"#,
                entity.to_bits(), unit_type_str, faction_str, pos.x, pos.y, hp
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
                r#"{{"x": {}, "y": {}, "type": "{}", "owner": "{}"}}"#,
                pos.x, pos.y, terrain_str, owner_str
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

    pub fn execute_ai_turn(&mut self) -> Promise {
        future_to_promise(async { Ok(JsValue::from_str("{}")) })
    }

    pub fn calculate_move_path(&self, unit_id: &str, dest_x: i32, dest_y: i32) -> Promise {
        let unit_id_owned = unit_id.to_string();
        future_to_promise(async move {
            let result_json = format!(r#"{{"unit_id": "{}", "path": [[0,0], [{}, {}]]}}"#, unit_id_owned, dest_x, dest_y);
            Ok(JsValue::from_str(&result_json))
        })
    }
}
