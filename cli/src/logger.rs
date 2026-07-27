use bevy_ecs::prelude::*;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;

use engine::components::*;
use engine::events::*;
use engine::resources::*;

/// ログレコードの共通ヘッダー構造体
#[derive(Debug, Serialize, PartialEq)]
pub struct LogRecord {
    pub turn: u32,
    pub player: u32,
    pub event: String,
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// 盤面スナップショットのユニット情報
#[derive(Debug, Serialize, PartialEq)]
pub struct UnitSnapshot {
    pub entity_index: u32,
    pub player: u32,
    pub unit_type: String,
    pub x: usize,
    pub y: usize,
    pub hp: u32,
    pub fuel: u32,
    pub ammo1: u32,
    pub ammo2: u32,
}

/// 盤面スナップショットの拠点情報
#[derive(Debug, Serialize, PartialEq)]
pub struct PropertySnapshot {
    pub x: usize,
    pub y: usize,
    pub owner: Option<u32>,
}

/// 盤面スナップショット全体のペイロード
#[derive(Debug, Serialize, PartialEq)]
pub struct SnapshotPayload {
    pub players_funds: Vec<(u32, u32)>,
    pub units: Vec<UnitSnapshot>,
    pub properties: Vec<PropertySnapshot>,
}

/// 対戦ロガー
pub struct BattleLogger {
    pub file_path: String,
}

#[allow(dead_code)]
impl BattleLogger {
    pub fn new(file_path: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
        }
    }

    /// レコードをファイルへ1行追記
    fn write_record(&self, record: &LogRecord) -> std::io::Result<()> {
        let json_line = serde_json::to_string(record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)?;
        writeln!(file, "{}", json_line)?;
        Ok(())
    }

    /// 複数のレコードを一度にファイルへ追記
    fn write_records(&self, records: &[LogRecord]) -> std::io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)?;
        for record in records {
            let json_line = serde_json::to_string(record)?;
            writeln!(file, "{}", json_line)?;
        }
        Ok(())
    }

    /// ワールドからスナップショットを生成しログに出力
    pub fn log_snapshot(&self, world: &mut World) -> std::io::Result<()> {
        let (turn, active_player) = {
            let match_state = world.get_resource::<MatchState>();
            let players = world.get_resource::<Players>();
            if let (Some(ms), Some(ps)) = (match_state, players) {
                let turn = ms.current_turn_number.0;
                let player =
                    ps.0.get(ms.active_player_index.0)
                        .map(|p| p.id.0)
                        .unwrap_or(0);
                (turn, player)
            } else {
                (0, 0)
            }
        };

        let players_funds = {
            let players = world.get_resource::<Players>();
            players
                .map(|ps| ps.0.iter().map(|p| (p.id.0, p.funds)).collect())
                .unwrap_or_default()
        };

        let mut units = Vec::new();
        {
            let mut query = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                &Health,
                Option<&Fuel>,
                Option<&Ammo>,
                Option<&Transporting>,
            )>();
            for (entity, pos, faction, stats, health, fuel, ammo, trans) in query.iter(world) {
                if trans.is_some() {
                    continue; // 輸送中のユニットは除外または別扱い
                }
                units.push(UnitSnapshot {
                    entity_index: entity.index(),
                    player: faction.0.0,
                    unit_type: format!("{:?}", stats.unit_type),
                    x: pos.x,
                    y: pos.y,
                    hp: health.current,
                    fuel: fuel.map(|f| f.current).unwrap_or(0),
                    ammo1: ammo.map(|a| a.ammo1).unwrap_or(0),
                    ammo2: ammo.map(|a| a.ammo2).unwrap_or(0),
                });
            }
        }

        let mut properties = Vec::new();
        {
            let mut query = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in query.iter(world) {
                properties.push(PropertySnapshot {
                    x: pos.x,
                    y: pos.y,
                    owner: prop.owner_id.map(|p| p.0),
                });
            }
        }

        let payload = serde_json::to_value(SnapshotPayload {
            players_funds,
            units,
            properties,
        })
        .unwrap_or(serde_json::Value::Null);

        let record = LogRecord {
            turn,
            player: active_player,
            event: "TurnSnapshot".to_string(),
            payload,
        };

        self.write_record(&record)
    }

    /// 各種イベントをチェックしてログに出力
    pub fn process_events(&self, world: &mut World) -> std::io::Result<()> {
        let (turn, active_player) = {
            let match_state = world.get_resource::<MatchState>();
            let players = world.get_resource::<Players>();
            if let (Some(ms), Some(ps)) = (match_state, players) {
                let turn = ms.current_turn_number.0;
                let player =
                    ps.0.get(ms.active_player_index.0)
                        .map(|p| p.id.0)
                        .unwrap_or(0);
                (turn, player)
            } else {
                (0, 0)
            }
        };

        let mut records = Vec::new();

        // 移動イベント
        if let Some(events) = world.get_resource::<Events<UnitMovedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "entity": ev.entity.index(),
                    "from": [ev.from.x, ev.from.y],
                    "to": [ev.to.x, ev.to.y],
                    "fuel_used": ev.fuel_used,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitMoved".to_string(),
                    payload,
                });
            }
        }

        // 攻撃イベント
        if let Some(events) = world.get_resource::<Events<UnitAttackedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "attacker": ev.attacker.index(),
                    "defender": ev.defender.index(),
                    "damage_dealt": ev.damage_dealt,
                    "counter_damage_dealt": ev.counter_damage_dealt,
                    "attacker_hp_before": ev.attacker_hp_before,
                    "attacker_hp_after": ev.attacker_hp_after,
                    "defender_hp_before": ev.defender_hp_before,
                    "defender_hp_after": ev.defender_hp_after,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitAttacked".to_string(),
                    payload,
                });
            }
        }

        // 撃破イベント
        if let Some(events) = world.get_resource::<Events<UnitDestroyedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "entity": ev.entity.index(),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitDestroyed".to_string(),
                    payload,
                });
            }
        }

        // 占領イベント
        if let Some(events) = world.get_resource::<Events<PropertyCapturedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "x": ev.x,
                    "y": ev.y,
                    "new_owner": ev.new_owner.map(|p| p.0),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "PropertyCaptured".to_string(),
                    payload,
                });
            }
        }

        // 占領進行イベント
        if let Some(events) = world.get_resource::<Events<PropertyCaptureProgressedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "unit": ev.unit.to_bits(),
                    "x": ev.x,
                    "y": ev.y,
                    "previous_capture_points": ev.previous_capture_points,
                    "remaining_capture_points": ev.remaining_capture_points,
                    "completed": ev.completed,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "PropertyCaptureProgressed".to_string(),
                    payload,
                });
            }
        }

        // 生産イベント
        if let Some(events) = world.get_resource::<Events<UnitProducedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "entity": ev.entity.index(),
                    "unit_type": format!("{:?}", ev.unit_type),
                    "x": ev.target_x,
                    "y": ev.target_y,
                });
                records.push(LogRecord {
                    turn,
                    player: ev.player_id.0,
                    event: "UnitProduced".to_string(),
                    payload,
                });
            }
        }

        // 合流イベント
        if let Some(events) = world.get_resource::<Events<UnitMergedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "source": ev.source_entity.index(),
                    "target": ev.target_entity.index(),
                    "refunded_funds": ev.refunded_funds,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitMerged".to_string(),
                    payload,
                });
            }
        }

        // 補給イベント
        if let Some(events) = world.get_resource::<Events<UnitSuppliedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "supplier": ev.supplier.index(),
                    "target": ev.target.index(),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitSupplied".to_string(),
                    payload,
                });
            }
        }

        // 積載イベント
        if let Some(events) = world.get_resource::<Events<UnitLoadedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "transport": ev.transport.index(),
                    "cargo": ev.cargo.index(),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitLoaded".to_string(),
                    payload,
                });
            }
        }

        // 降車イベント
        if let Some(events) = world.get_resource::<Events<UnitUnloadedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "transport": ev.transport.index(),
                    "cargo": ev.cargo.index(),
                    "x": ev.target_x,
                    "y": ev.target_y,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitUnloaded".to_string(),
                    payload,
                });
            }
        }

        // 待機イベント
        if let Some(events) = world.get_resource::<Events<UnitWaitedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "entity": ev.entity.index(),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "UnitWaited".to_string(),
                    payload,
                });
            }
        }

        // AI思考評価イベント
        if let Some(events) = world.get_resource::<Events<AiActionEvaluatedEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "entity": ev.entity.index(),
                    "mission_type": ev.mission_type,
                    "action_type": ev.action_type,
                    "score": ev.score,
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "AiActionEvaluated".to_string(),
                    payload,
                });
            }
        }

        // ゲームオーバーイベント
        if let Some(events) = world.get_resource::<Events<GameOverEvent>>() {
            let mut cursor = events.get_cursor();
            for ev in cursor.read(events) {
                let payload = serde_json::json!({
                    "condition": format!("{:?}", ev.condition),
                });
                records.push(LogRecord {
                    turn,
                    player: active_player,
                    event: "GameOver".to_string(),
                    payload,
                });
            }
        }

        self.write_records(&records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_record_serialization() {
        let record = LogRecord {
            turn: 1,
            player: 1,
            event: "UnitMoved".to_string(),
            payload: serde_json::json!({
                "entity": 10,
                "from": [0, 0],
                "to": [1, 1],
                "fuel_used": 2,
            }),
        };

        let json_str = serde_json::to_string(&record).unwrap();
        assert!(json_str.contains("\"turn\":1"));
        assert!(json_str.contains("\"player\":1"));
        assert!(json_str.contains("\"event\":\"UnitMoved\""));
        assert!(json_str.contains("\"fuel_used\":2"));
    }
}
