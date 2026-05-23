#![allow(clippy::collapsible_if)]

use crate::components::{Ammo, Faction, GridPosition, Health};
use bevy_ecs::prelude::*;
use std::collections::HashSet;

/// ミッションの種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionType {
    Attack,
    Capture,
    Defense,
    Transport,
}

/// 輸送ミッションの各フェーズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    Pickup,
    Transit,
    Drop,
    Return,
}

/// 部隊が実行中のミッションの状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionPhase {
    Forming,
    MovingToTarget,
    Executing,
    Completed,
    Transport(TransportPhase),
}

/// 部隊（Squad）の定義
#[derive(Debug, Clone)]
pub struct Squad {
    pub id: u32,
    pub members: HashSet<Entity>,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>, // 攻撃・防衛・占領の目標座標
    pub phase: MissionPhase,
    pub transport_cargo: Option<Entity>, // 輸送対象（TransportMission用）
}

/// 全ての部隊を管理するリソース
#[derive(Resource, Default, Debug, Clone)]
pub struct SquadManager {
    pub squads: Vec<Squad>,
    pub solo_fallbacks: HashSet<Entity>,
    next_id: u32,
}

impl SquadManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_squad(&mut self, mission_type: MissionType) -> &mut Squad {
        let squad = Squad {
            id: self.next_id,
            members: HashSet::new(),
            mission_type,
            target: None,
            phase: MissionPhase::Forming,
            transport_cargo: None,
        };
        self.next_id += 1;
        self.squads.push(squad);
        self.squads.last_mut().unwrap()
    }

    pub fn remove_squad(&mut self, id: u32) {
        self.squads.retain(|s| s.id != id);
    }
}

/// 毎ターンの部隊の再編成と SoloFallback の判定を行います。
pub fn update_squads(world: &mut World, perspective_player: crate::components::PlayerId) {
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();

    // 存在しなくなったエンティティの削除
    let mut existing_entities = HashSet::new();
    let mut units_needing_fallback = Vec::new();
    let mut units_recovered = Vec::new();

    let mut query = world.query::<(Entity, &Faction, &Health, Option<&Ammo>)>();
    for (entity, faction, health, ammo_opt) in query.iter(world) {
        if faction.0 == perspective_player {
            existing_entities.insert(entity);

            // SoloFallback の判定 (HP < 60 または 弾薬切れ)
            let mut no_ammo = false;
            if let Some(ammo) = ammo_opt {
                no_ammo = (ammo.max_ammo1 > 0 && ammo.ammo1 == 0) && (ammo.max_ammo2 > 0 && ammo.ammo2 == 0);
            }

            if health.current < 60 || no_ammo {
                units_needing_fallback.push(entity);
            } else if health.current >= 70 && !no_ammo {
                // 回復条件を満たした
                units_recovered.push(entity);
            }
        }
    }

    for squad in &mut manager.squads {
        squad.members.retain(|e| existing_entities.contains(e));
        if let Some(cargo) = squad.transport_cargo {
            if !existing_entities.contains(&cargo) {
                squad.transport_cargo = None;
            }
        }
    }

    // SoloFallback の更新
    manager.solo_fallbacks.retain(|e| existing_entities.contains(e));

    for e in units_needing_fallback {
        manager.solo_fallbacks.insert(e);
        // Squad から外す
        for squad in &mut manager.squads {
            squad.members.remove(&e);
        }
    }

    for e in units_recovered {
        manager.solo_fallbacks.remove(&e);
    }

    // メンバーが0になった部隊の解散
    manager.squads.retain(|s| !s.members.is_empty() || s.phase == MissionPhase::Forming);

    world.insert_resource(manager);
}
