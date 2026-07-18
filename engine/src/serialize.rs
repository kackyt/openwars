use crate::components::*;
use crate::resources::*;
use base64::prelude::*;
use bevy_ecs::prelude::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("形式不正: {0}")]
    InvalidFormat(String),

    #[error("Base64デコード失敗: {0}")]
    Base64DecodeError(#[from] base64::DecodeError),

    #[error("JSONデコード失敗: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error(
        "署名検証失敗: 署名が一致しません。データが破損しているか、改ざんされた可能性があります。"
    )]
    SignatureMismatch,

    #[error("マスターデータ不整合: {0}")]
    MasterDataMismatch(String),

    #[error("リソースが見つかりません: {0}")]
    ResourceNotFound(String),

    #[error("HMAC初期化失敗: {0}")]
    HmacInitError(String),
}

fn get_hmac_key() -> &'static [u8] {
    option_env!("OPENWARS_HMAC_KEY")
        .map(|k| k.as_bytes())
        .unwrap_or(b"openwars-default-secret-key-fallback-32bytes-long!")
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct UnitSave {
    pub id: uuid::Uuid,
    pub unit_type: crate::resources::UnitType,
    pub x: usize,
    pub y: usize,
    pub faction_id: u32,
    pub health: u32,
    pub max_health: u32,
    pub fuel: Option<(u32, u32)>, // current, max
    pub ammo: Option<AmmoSave>,
    pub loaded_ids: Vec<uuid::Uuid>, // 搭載されているユニットの Uuid
    pub has_moved: bool,
    pub action_completed: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct AmmoSave {
    pub ammo1: u32,
    pub max_ammo1: u32,
    pub ammo2: u32,
    pub max_ammo2: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct PropertySave {
    pub x: usize,
    pub y: usize,
    pub terrain: crate::resources::Terrain,
    pub owner_id: Option<u32>,
    pub capture_points: u32,
    pub max_capture_points: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct PlayerSave {
    pub id: u32,
    pub name: String,
    pub funds: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct MatchStateSave {
    pub current_turn_number: u32,
    pub active_player_index: usize,
    pub current_phase: PhaseSave,
    pub game_over: Option<GameOverConditionSave>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum PhaseSave {
    Main,
    EndTurn,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum GameOverConditionSave {
    Winner(u32),
    Draw,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct CombatRecordSave {
    pub value_dealt: i64,
    pub value_received: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct CombatLedgerSave {
    pub records: HashMap<u32, CombatRecordSave>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct SaveState {
    pub map_name: String,
    pub map_width: usize,
    pub map_height: usize,
    pub map_tiles: Vec<crate::resources::Terrain>,
    pub map_topology: crate::resources::GridTopology,
    pub match_state: MatchStateSave,
    pub players: Vec<PlayerSave>,
    pub properties: Vec<PropertySave>,
    pub units: Vec<UnitSave>,
    pub rng_seed: u64,
    pub combat_ledger: CombatLedgerSave,
    pub ai_action_cooldown: Vec<uuid::Uuid>,
    pub ai_production_cooldown: Vec<(usize, usize)>,
}

/// ワールドの状態を HMAC 署名付きの Base64 文字列（OPWS1.base64.signature）にエクスポートします。
pub fn export_save_data(world: &mut World, map_name: &str) -> Result<String, SaveError> {
    // 1. 各リソースからのデータ退避（借用の競合を避けるためブロック内でコピー/クローン）
    let (map_width, map_height, map_tiles, map_topology) = {
        let map = world
            .get_resource::<Map>()
            .ok_or_else(|| SaveError::ResourceNotFound("Map".to_string()))?;
        (map.width, map.height, map.tiles.clone(), map.topology)
    };

    let match_state = {
        let match_state_res = world
            .get_resource::<MatchState>()
            .ok_or_else(|| SaveError::ResourceNotFound("MatchState".to_string()))?;
        MatchStateSave {
            current_turn_number: match_state_res.current_turn_number.0,
            active_player_index: match_state_res.active_player_index.0,
            current_phase: match match_state_res.current_phase {
                Phase::Main => PhaseSave::Main,
                Phase::EndTurn => PhaseSave::EndTurn,
            },
            game_over: match_state_res.game_over.as_ref().map(|g| match g {
                GameOverCondition::Winner(pid) => GameOverConditionSave::Winner(pid.0),
                GameOverCondition::Draw => GameOverConditionSave::Draw,
            }),
        }
    };

    let players = {
        let players_res = world
            .get_resource::<Players>()
            .ok_or_else(|| SaveError::ResourceNotFound("Players".to_string()))?;
        players_res
            .0
            .iter()
            .map(|p| PlayerSave {
                id: p.id.0,
                name: p.name.clone(),
                funds: p.funds,
            })
            .collect::<Vec<_>>()
    };

    let rng_seed = {
        let rng_res = world
            .get_resource::<GameRng>()
            .ok_or_else(|| SaveError::ResourceNotFound("GameRng".to_string()))?;
        rng_res.seed
    };

    let combat_ledger = {
        let combat_ledger_res = world
            .get_resource::<CombatLedger>()
            .ok_or_else(|| SaveError::ResourceNotFound("CombatLedger".to_string()))?;
        let records = combat_ledger_res
            .records
            .iter()
            .map(|(pid, record)| {
                (
                    pid.0,
                    CombatRecordSave {
                        value_dealt: record.value_dealt,
                        value_received: record.value_received,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        CombatLedgerSave { records }
    };

    // 物件とユニットをクエリで取得（これらは &mut World を借用します）
    let mut properties = Vec::new();
    let mut entity_to_uuid = HashMap::new();
    let mut units = Vec::new();
    {
        let mut p_query = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in p_query.iter(world) {
            properties.push(PropertySave {
                x: pos.x,
                y: pos.y,
                terrain: prop.terrain,
                owner_id: prop.owner_id.map(|o| o.0),
                capture_points: prop.capture_points,
                max_capture_points: prop.max_capture_points,
            });
        }

        let mut u_query = world.query::<(
            Entity,
            &UnitId,
            &Faction,
            &GridPosition,
            &Health,
            &UnitStats,
            Option<&Fuel>,
            Option<&Ammo>,
            Option<&CargoCapacity>,
            &HasMoved,
            &ActionCompleted,
            Option<&Transporting>,
        )>();

        // Entity と Uuid のマッピングテーブルを構築
        for (entity, id, _, _, _, _, _, _, _, _, _, _) in u_query.iter(world) {
            entity_to_uuid.insert(entity, id.0);
        }

        // 各ユニットをシリアライズ
        for (
            _entity,
            id,
            faction,
            pos,
            health,
            stats,
            fuel,
            ammo,
            cargo,
            has_moved,
            completed,
            _transporting,
        ) in u_query.iter(world)
        {
            let loaded_ids = cargo
                .map(|c| {
                    c.loaded
                        .iter()
                        .filter_map(|e| entity_to_uuid.get(e).copied())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            units.push(UnitSave {
                id: id.0,
                unit_type: stats.unit_type,
                x: pos.x,
                y: pos.y,
                faction_id: faction.0.0,
                health: health.current,
                max_health: health.max,
                fuel: fuel.map(|f| (f.current, f.max)),
                ammo: ammo.map(|a| AmmoSave {
                    ammo1: a.ammo1,
                    max_ammo1: a.max_ammo1,
                    ammo2: a.ammo2,
                    max_ammo2: a.max_ammo2,
                }),
                loaded_ids,
                has_moved: has_moved.0,
                action_completed: completed.0,
            });
        }
    } // クエリの借用はここで終了

    // クエリ終了後、再度リソースを借用して AI のクールダウン情報を抽出
    let ai_action_cooldown =
        if let Some(res) = world.get_resource::<crate::ai::engine::AiActionCooldown>() {
            res.0
                .iter()
                .filter_map(|e| entity_to_uuid.get(e).copied())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

    let ai_production_cooldown =
        if let Some(res) = world.get_resource::<crate::ai::engine::AiProductionCooldown>() {
            res.0.iter().copied().collect::<Vec<_>>()
        } else {
            Vec::new()
        };

    // 2. SaveState オブジェクトの構築
    let save_state = SaveState {
        map_name: map_name.to_string(),
        map_width,
        map_height,
        map_tiles,
        map_topology,
        match_state,
        players,
        properties,
        units,
        rng_seed,
        combat_ledger,
        ai_action_cooldown,
        ai_production_cooldown,
    };

    // 3. JSON シリアライズと Base64 エンコード
    let json_str = serde_json::to_string(&save_state)?;
    let base64_data = BASE64_STANDARD.encode(json_str.as_bytes());

    // 4. HMAC 署名の計算
    let mut mac = HmacSha256::new_from_slice(get_hmac_key())
        .map_err(|e| SaveError::HmacInitError(e.to_string()))?;
    mac.update(base64_data.as_bytes());
    let result = mac.finalize();
    let signature = BASE64_STANDARD.encode(result.into_bytes());

    // 5. 結合したテキストを返却
    Ok(format!("OPWS1.{}.{}", base64_data, signature))
}

/// 署名付きの Base64 文字列（OPWS1.base64.signature）からワールドを復元します。
pub fn import_save_data(
    save_str: &str,
    master_data: &MasterDataRegistry,
) -> Result<(World, Schedule), SaveError> {
    let parts: Vec<&str> = save_str.split('.').collect();
    if parts.len() != 3 {
        return Err(SaveError::InvalidFormat(
            "Expected 3 segments separated by dots.".to_string(),
        ));
    }

    let header = parts[0];
    let base64_data = parts[1];
    let signature_str = parts[2];

    if header != "OPWS1" {
        return Err(SaveError::InvalidFormat(format!(
            "Unsupported save data version or header: {}",
            header
        )));
    }

    // 1. 署名検証
    let mut mac = HmacSha256::new_from_slice(get_hmac_key())
        .map_err(|e| SaveError::HmacInitError(e.to_string()))?;
    mac.update(base64_data.as_bytes());

    let decoded_signature = BASE64_STANDARD
        .decode(signature_str.as_bytes())
        .map_err(SaveError::Base64DecodeError)?;

    mac.verify_slice(&decoded_signature)
        .map_err(|_| SaveError::SignatureMismatch)?;

    // 2. Base64 デコードと JSON デシリアライズ
    let decoded_json_bytes = BASE64_STANDARD
        .decode(base64_data.as_bytes())
        .map_err(SaveError::Base64DecodeError)?;
    let save_state: SaveState = serde_json::from_slice(&decoded_json_bytes)?;

    // 3. World と Schedule の構築
    let (mut world, schedule) = crate::setup::create_world();

    // DamageChart と UnitRegistry をマスターデータから再構築して登録
    let mut damage_chart = DamageChart::new();
    for (unit_name, unit_record) in &master_data.units {
        let att_type = master_data
            .unit_type_for_name(&unit_name.0)
            .map_err(|e| SaveError::MasterDataMismatch(format!("Unit type not found: {:?}", e)))?;

        if let Some(w1_name) = &unit_record.weapon1 {
            let weapon = master_data
                .weapons
                .get(&crate::resources::master_data::UnitName(w1_name.clone()))
                .ok_or_else(|| {
                    SaveError::MasterDataMismatch(format!("Weapon not found: {}", w1_name))
                })?;
            for (def_name, dmg) in &weapon.damages {
                let def_type = master_data.unit_type_for_name(def_name).map_err(|e| {
                    SaveError::MasterDataMismatch(format!("Unit type not found: {:?}", e))
                })?;
                damage_chart.insert_damage(att_type, def_type, *dmg);
            }
        }
        if let Some(w2_name) = &unit_record.weapon2 {
            let weapon = master_data
                .weapons
                .get(&crate::resources::master_data::UnitName(w2_name.clone()))
                .ok_or_else(|| {
                    SaveError::MasterDataMismatch(format!("Weapon not found: {}", w2_name))
                })?;
            for (def_name, dmg) in &weapon.damages {
                let def_type = master_data.unit_type_for_name(def_name).map_err(|e| {
                    SaveError::MasterDataMismatch(format!("Unit type not found: {:?}", e))
                })?;
                damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
            }
        }
    }
    world.insert_resource(damage_chart);

    let mut unit_registry_map = std::collections::HashMap::new();
    for name in master_data.units.keys() {
        let stats = master_data
            .create_unit_stats(name)
            .map_err(|e| SaveError::MasterDataMismatch(format!("Master data error: {:?}", e)))?;
        unit_registry_map.insert(stats.unit_type, stats);
    }
    world.insert_resource(UnitRegistry(unit_registry_map));
    world.insert_resource(GameRng::new(save_state.rng_seed));

    // マップリソースと IslandMap の構築
    let ecs_map = Map {
        width: save_state.map_width,
        height: save_state.map_height,
        tiles: save_state.map_tiles.clone(),
        topology: save_state.map_topology,
    };
    let island_map = crate::ai::islands::IslandMap::analyze(&ecs_map);
    world.insert_resource(ecs_map);
    world.insert_resource(island_map);

    // プレイヤー情報の復元
    let players = save_state
        .players
        .iter()
        .map(|p| Player {
            id: PlayerId(p.id),
            name: p.name.clone(),
            funds: p.funds,
        })
        .collect::<Vec<_>>();
    world.insert_resource(Players(players));

    // MatchState の復元
    let match_state = MatchState {
        current_turn_number: TurnNumber(save_state.match_state.current_turn_number),
        active_player_index: PlayerIndex(save_state.match_state.active_player_index),
        current_phase: match save_state.match_state.current_phase {
            PhaseSave::Main => Phase::Main,
            PhaseSave::EndTurn => Phase::EndTurn,
        },
        game_over: save_state.match_state.game_over.map(|g| match g {
            GameOverConditionSave::Winner(pid) => GameOverCondition::Winner(PlayerId(pid)),
            GameOverConditionSave::Draw => GameOverCondition::Draw,
        }),
    };
    world.insert_resource(match_state);
    world.insert_resource(master_data.clone());

    // CombatLedger の復元
    let mut ledger_records = HashMap::new();
    for (pid_val, rec) in &save_state.combat_ledger.records {
        ledger_records.insert(
            PlayerId(*pid_val),
            CombatRecord {
                value_dealt: rec.value_dealt,
                value_received: rec.value_received,
            },
        );
    }
    world.insert_resource(CombatLedger {
        records: ledger_records,
    });

    // 物件 (Property) エンティティの再生成
    for prop in &save_state.properties {
        let terrain = prop.terrain;
        let owner = prop.owner_id.map(PlayerId);
        let mut ecs_prop = Property::new(terrain, owner, prop.max_capture_points);
        ecs_prop.capture_points = prop.capture_points;
        world.spawn((
            GridPosition {
                x: prop.x,
                y: prop.y,
            },
            ecs_prop,
        ));
    }

    // ユニットエンティティの spawn と Uuid から Entity ID へのマッピングテーブル構築
    let mut uuid_to_entity = HashMap::new();
    let mut unit_cargos_to_resolve = Vec::new();

    let registry = world.resource::<UnitRegistry>().clone();

    for u in &save_state.units {
        let stats = registry
            .get_stats(u.unit_type)
            .ok_or_else(|| {
                SaveError::MasterDataMismatch(format!(
                    "Stats not found for unit type: {:?}",
                    u.unit_type
                ))
            })?
            .clone();

        let mut cmd = world.spawn_empty();
        let entity = cmd.id();
        uuid_to_entity.insert(u.id, entity);

        let max_cargo = stats.max_cargo;

        cmd.insert((
            UnitId(u.id),
            Faction(PlayerId(u.faction_id)),
            GridPosition { x: u.x, y: u.y },
            Health {
                current: u.health,
                max: u.max_health,
            },
            stats,
            HasMoved(u.has_moved),
            ActionCompleted(u.action_completed),
        ));

        if max_cargo > 0 {
            cmd.insert(CargoCapacity {
                max: max_cargo,
                loaded: Vec::new(),
            });
        }

        if let Some((curr, max)) = u.fuel {
            cmd.insert(Fuel { current: curr, max });
        }
        if let Some(ref a) = u.ammo {
            cmd.insert(Ammo {
                ammo1: a.ammo1,
                max_ammo1: a.max_ammo1,
                ammo2: a.ammo2,
                max_ammo2: a.max_ammo2,
            });
        }

        // あとで子ユニットの Entity ID に解決するための情報を保持
        if !u.loaded_ids.is_empty() {
            unit_cargos_to_resolve.push((entity, u.loaded_ids.clone()));
        }
    }

    // ユニットの搭載関係 (CargoCapacity, Transporting) を Entity ID に解決して付与
    for (parent_entity, loaded_uuids) in unit_cargos_to_resolve {
        let mut loaded_entities = Vec::new();
        for child_uuid in &loaded_uuids {
            if let Some(child_entity) = uuid_to_entity.get(child_uuid).copied() {
                loaded_entities.push(child_entity);
                // 子ユニットに Transporting コンポーネントを追加
                world
                    .entity_mut(child_entity)
                    .insert(Transporting(parent_entity));
            }
        }
        // すでに CargoCapacity が入っているはずなので、それを取得して loaded を更新する
        if let Some(mut cargo) = world.entity_mut(parent_entity).get_mut::<CargoCapacity>() {
            cargo.loaded = loaded_entities;
        }
    }

    // AIクールダウンの復元
    let ai_action_cooldown_entities = save_state
        .ai_action_cooldown
        .iter()
        .filter_map(|uuid| uuid_to_entity.get(uuid).copied())
        .collect::<HashSet<_>>();
    world.insert_resource(crate::ai::engine::AiActionCooldown(
        ai_action_cooldown_entities,
    ));

    let ai_production_cooldown_coords = save_state
        .ai_production_cooldown
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    world.insert_resource(crate::ai::engine::AiProductionCooldown(
        ai_production_cooldown_coords,
    ));

    Ok((world, schedule))
}

#[derive(Debug, Clone)]
pub struct SaveHeader {
    pub map_name: String,
    pub turn_number: u32,
    pub active_player_name: String,
}

/// セーブデータ文字列から署名検証を行わずにヘッダー情報を読み取ります。
/// メニュー表示などの目的で高速に情報を読み取るために使用します。
pub fn read_save_header(save_str: &str) -> Result<SaveHeader, SaveError> {
    let parts: Vec<&str> = save_str.split('.').collect();
    if parts.len() != 3 || parts[0] != "OPWS1" {
        return Err(SaveError::InvalidFormat(
            "Invalid save data format".to_string(),
        ));
    }
    let base64_data = parts[1];
    let decoded_json_bytes = BASE64_STANDARD
        .decode(base64_data.as_bytes())
        .map_err(SaveError::Base64DecodeError)?;
    let val: serde_json::Value = serde_json::from_slice(&decoded_json_bytes)?;

    let map_name = val
        .get("map_name")
        .and_then(|v| v.as_str())
        .unwrap_or("不明")
        .to_string();
    let turn_number = val
        .get("match_state")
        .and_then(|v| v.get("current_turn_number"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let active_idx = val
        .get("match_state")
        .and_then(|v| v.get("active_player_index"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let active_player_name = val
        .get("players")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(active_idx))
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("不明")
        .to_string();

    Ok(SaveHeader {
        map_name,
        turn_number,
        active_player_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::MasterDataRegistry;

    #[test]
    fn test_save_load_loop() {
        let master_data = MasterDataRegistry::load().expect("Failed to load master data");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("Failed to initialize world");

        // テスト用のユニットをスポーン
        let unit_id = uuid::Uuid::new_v4();
        let stats = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                "軽歩兵".to_string(),
            ))
            .unwrap();
        world.spawn((
            UnitId(unit_id),
            Faction(PlayerId(1)),
            GridPosition { x: 1, y: 1 },
            Health {
                current: 95,
                max: 100,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            stats,
            HasMoved(false),
            ActionCompleted(false),
        ));

        // 1. エクスポート
        let exported_str = export_save_data(&mut world, "map_1").expect("Export failed");
        assert!(exported_str.starts_with("OPWS1."));

        // 2. インポート
        let (mut imported_world, _imported_schedule) =
            import_save_data(&exported_str, &master_data).expect("Import failed");

        // 3. 各リソースの検証
        let original_match = world.get_resource::<MatchState>().unwrap();
        let imported_match = imported_world.get_resource::<MatchState>().unwrap();
        assert_eq!(
            original_match.current_turn_number,
            imported_match.current_turn_number
        );
        assert_eq!(
            original_match.active_player_index,
            imported_match.active_player_index
        );
        assert_eq!(original_match.current_phase, imported_match.current_phase);

        let original_players = world.get_resource::<Players>().unwrap();
        let imported_players = imported_world.get_resource::<Players>().unwrap();
        assert_eq!(original_players.0.len(), imported_players.0.len());
        for i in 0..original_players.0.len() {
            assert_eq!(original_players.0[i].id, imported_players.0[i].id);
            assert_eq!(original_players.0[i].name, imported_players.0[i].name);
            assert_eq!(original_players.0[i].funds, imported_players.0[i].funds);
        }

        let original_map = world.get_resource::<Map>().unwrap();
        let imported_map = imported_world.get_resource::<Map>().unwrap();
        assert_eq!(original_map.width, imported_map.width);
        assert_eq!(original_map.height, imported_map.height);
        assert_eq!(original_map.tiles, imported_map.tiles);
        assert_eq!(original_map.topology, imported_map.topology);

        // 4. ユニットが正しく復元されていることの検証
        let mut u_query = imported_world.query::<(&UnitId, &Faction, &GridPosition, &Health)>();
        let imported_units: Vec<_> = u_query.iter(&imported_world).collect();
        assert_eq!(imported_units.len(), 1);
        let (imp_id, imp_fac, imp_pos, imp_hp) = imported_units[0];
        assert_eq!(imp_id.0, unit_id);
        assert_eq!(imp_fac.0.0, 1);
        assert_eq!(imp_pos.x, 1);
        assert_eq!(imp_pos.y, 1);
        assert_eq!(imp_hp.current, 95);
    }

    #[test]
    fn test_signature_tamper_detection() {
        let master_data = MasterDataRegistry::load().expect("Failed to load master data");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("Failed to initialize world");

        let exported_str = export_save_data(&mut world, "map_1").expect("Export failed");

        // データを改ざんする（Base64部分の末尾の文字を書き換えるなど）
        let parts: Vec<&str> = exported_str.split('.').collect();
        let mut tampered_base64 = parts[1].to_string();
        if let Some(last_char) = tampered_base64.pop() {
            // 文字を変更して差し替える
            let replacement = if last_char == 'A' { 'B' } else { 'A' };
            tampered_base64.push(replacement);
        }

        let tampered_str = format!("{}.{}.{}", parts[0], tampered_base64, parts[2]);

        let res = import_save_data(&tampered_str, &master_data);
        assert!(res.is_err());
        let err = res.err().unwrap();
        assert!(err.to_string().contains("署名検証失敗"));
    }

    #[test]
    fn test_cargo_capacity_serialization() {
        let master_data = MasterDataRegistry::load().expect("Failed to load master data");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("Failed to initialize world");

        // 1. 空の輸送ユニット（輸送ヘリ: max_cargo=1）と、積載用ユニット（軽歩兵）をスポーン
        let transport_id = uuid::Uuid::new_v4();
        let passenger_id = uuid::Uuid::new_v4();

        let transport_stats = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                "輸送ヘリ".to_string(),
            ))
            .unwrap();
        let passenger_stats = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                "軽歩兵".to_string(),
            ))
            .unwrap();

        // 空の輸送ヘリをスポーン
        let transport_entity = world
            .spawn((
                UnitId(transport_id),
                Faction(PlayerId(1)),
                GridPosition { x: 1, y: 1 },
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: transport_stats.max_fuel,
                    max: transport_stats.max_fuel,
                },
                transport_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
                CargoCapacity {
                    max: transport_stats.max_cargo,
                    loaded: Vec::new(),
                },
            ))
            .id();

        // 軽歩兵をスポーン
        let passenger_entity = world
            .spawn((
                UnitId(passenger_id),
                Faction(PlayerId(1)),
                GridPosition { x: 2, y: 2 },
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: passenger_stats.max_fuel,
                    max: passenger_stats.max_fuel,
                },
                passenger_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // 2. セーブ -> ロード
        let exported_str = export_save_data(&mut world, "map_1").expect("Export failed");
        let (mut imported_world, _imported_schedule) =
            import_save_data(&exported_str, &master_data).expect("Import failed");

        // 3. ロード後の空の輸送ヘリが CargoCapacity を持っていることを検証
        let mut t_query = imported_world.query_filtered::<(Entity, &UnitId), With<CargoCapacity>>();
        let trans_list: Vec<_> = t_query.iter(&imported_world).collect();
        assert_eq!(trans_list.len(), 1);
        let (imp_transport_entity, imp_transport_id) = trans_list[0];
        assert_eq!(imp_transport_id.0, transport_id);

        let cargo = imported_world
            .get::<CargoCapacity>(imp_transport_entity)
            .unwrap();
        assert_eq!(cargo.max, transport_stats.max_cargo);
        assert!(cargo.loaded.is_empty());

        // 4. 今度は積載した状態でセーブ -> ロード
        // passenger_entity を transport_entity に搭載する
        world.entity_mut(transport_entity).insert(CargoCapacity {
            max: transport_stats.max_cargo,
            loaded: vec![passenger_entity],
        });
        world
            .entity_mut(passenger_entity)
            .insert(Transporting(transport_entity));

        let exported_str2 = export_save_data(&mut world, "map_1").expect("Export failed");
        let (mut imported_world2, _imported_schedule2) =
            import_save_data(&exported_str2, &master_data).expect("Import failed");

        // 5. ロード後に積載関係が正しく復元されていることを検証
        let mut u_query = imported_world2.query::<(
            Entity,
            &UnitId,
            Option<&CargoCapacity>,
            Option<&Transporting>,
        )>();
        let mut imp_trans_cargo = None;
        let mut imp_pass_transporting = None;
        let mut imp_trans_entity = None;
        let mut imp_pass_entity = None;

        for (ent, uid, cargo, transporting) in u_query.iter(&imported_world2) {
            if uid.0 == transport_id {
                imp_trans_cargo = cargo.cloned();
                imp_trans_entity = Some(ent);
            } else if uid.0 == passenger_id {
                imp_pass_transporting = transporting.cloned();
                imp_pass_entity = Some(ent);
            }
        }

        let imp_trans_cargo = imp_trans_cargo.expect("CargoCapacity missing");
        let imp_pass_transporting = imp_pass_transporting.expect("Transporting missing");
        let imp_trans_entity = imp_trans_entity.unwrap();
        let imp_pass_entity = imp_pass_entity.unwrap();

        assert_eq!(imp_trans_cargo.max, transport_stats.max_cargo);
        assert_eq!(imp_trans_cargo.loaded, vec![imp_pass_entity]);
        assert_eq!(imp_pass_transporting.0, imp_trans_entity);
    }
}
