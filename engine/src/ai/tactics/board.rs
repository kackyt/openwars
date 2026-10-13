//! 1回の行動選択で全unitが共有する盤面の事実。

use crate::ai::engine::{AiTacticalSnapshot, strategic_target_value};
use crate::ai::squad::{MissionType, SquadManager};
use crate::ai::threat::EnemyThreatInfo;
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, Terrain};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;

/// 物件の事実（位置・地形・所有者）。
pub(crate) type PropertyFact = (GridPosition, Terrain, Option<PlayerId>);

/// 1回の行動選択で全unitが共有する盤面の事実。
///
/// 命令を1件適用すると盤面が変わるため、次の行動選択では作り直す（step間で再利用しない）。
pub(crate) struct TacticalBoard {
    pub(crate) player_id: PlayerId,
    /// V3以降の戦術評価（地形防御・脅威圏・待ち伏せ）を使うか。
    pub(crate) uses_v3_tactics: bool,
    pub(crate) map: Map,
    pub(crate) registry: MasterDataRegistry,
    pub(crate) damage_chart: DamageChart,
    /// 輸送中を除く全unitの占有マス（移動・距離計算の通行判定に使う）。
    pub(crate) occupancy: HashMap<(usize, usize), OccupantInfo>,
    pub(crate) properties: Vec<PropertyFact>,
    pub(crate) unit_costs: HashMap<Entity, u32>,
    /// 輸送中を除く生存敵unit（位置・兵種・戦略価値・HP・射程・移動力）。
    pub(crate) enemies: Vec<EnemyThreatInfo>,
    pub(crate) squads: SquadRoles,
    /// 移動先ごとの攻撃・合流・占領・待機の可否を引く索引。
    pub(crate) action_targets: AiTacticalSnapshot,
}

/// Squad編成から読む、各unitの任務と目標。
pub(crate) struct SquadRoles {
    pub(crate) manager: SquadManager,
    targets: HashMap<Entity, GridPosition>,
    missions: HashMap<Entity, MissionType>,
    solo_fallbacks: HashSet<Entity>,
}

impl SquadRoles {
    fn from_manager(manager: SquadManager) -> Self {
        let mut targets = HashMap::default();
        let mut missions = HashMap::default();
        for squad in &manager.squads {
            for &member in &squad.members {
                missions.insert(member, squad.mission_type.clone());
                if let Some(target) = squad.target {
                    targets.insert(member, target);
                }
            }
        }
        let solo_fallbacks = manager.solo_fallbacks.iter().copied().collect();
        Self {
            manager,
            targets,
            missions,
            solo_fallbacks,
        }
    }

    pub(crate) fn target(&self, entity: Entity) -> Option<GridPosition> {
        self.targets.get(&entity).copied()
    }

    pub(crate) fn mission(&self, entity: Entity) -> Option<&MissionType> {
        self.missions.get(&entity)
    }

    pub(crate) fn is_solo_fallback(&self, entity: Entity) -> bool {
        self.solo_fallbacks.contains(&entity)
    }

    /// 評価Event用の任務名（所属Squadの任務、単独行動、未所属）。
    pub(crate) fn mission_label(&self, entity: Entity) -> String {
        self.manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&entity))
            .map(|squad| format!("{:?}", squad.mission_type))
            .unwrap_or_else(|| {
                if self.manager.solo_fallbacks.contains(&entity) {
                    "SoloFallback".to_string()
                } else {
                    "Unknown".to_string()
                }
            })
    }
}

/// 行動可能な自軍unitと、輸送中を除く全unitの占有マスを集める。
pub(crate) fn movable_units_and_occupancy(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
) -> (Vec<Entity>, HashMap<(usize, usize), OccupantInfo>) {
    let mut movable_units = Vec::new();
    let mut occupancy = HashMap::default();
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &HasMoved,
        &ActionCompleted,
        &UnitStats,
        Option<&crate::components::CargoCapacity>,
        Option<&crate::components::Transporting>,
    )>();
    for (entity, pos, faction, has_moved, action_completed, stats, cargo, transporting) in
        query.iter(world)
    {
        if transporting.is_some() {
            continue;
        }
        if !skip_entities.contains(&entity)
            && faction.0 == player_id
            && !has_moved.0
            && !action_completed.0
        {
            movable_units.push(entity);
        }
        let free_slots = cargo
            .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
            .unwrap_or(0);
        occupancy.insert(
            (pos.x, pos.y),
            OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots,
            },
        );
    }
    (movable_units, occupancy)
}

impl TacticalBoard {
    pub(crate) fn observe(
        world: &mut World,
        player_id: PlayerId,
        occupancy: HashMap<(usize, usize), OccupantInfo>,
    ) -> Self {
        let uses_v3_tactics =
            crate::ai::resolve_player_ai_version(world, player_id).uses_v3_tactics();
        let squads = SquadRoles::from_manager(
            world
                .get_resource::<SquadManager>()
                .cloned()
                .unwrap_or_default(),
        );
        let map = world.resource::<Map>().clone();
        let registry = world.resource::<MasterDataRegistry>().clone();
        let action_targets = AiTacticalSnapshot::from_world(world, &registry, map.topology);
        let properties: Vec<PropertyFact> = world
            .query::<(&GridPosition, &Property)>()
            .iter(world)
            .map(|(position, property)| (*position, property.terrain, property.owner_id))
            .collect();
        let unit_costs: HashMap<Entity, u32> = world
            .query::<(Entity, &UnitStats)>()
            .iter(world)
            .map(|(entity, stats)| (entity, stats.cost))
            .collect();
        let enemies = observe_enemies(world, player_id, &properties, &unit_costs, &registry);
        let damage_chart = world.resource::<DamageChart>().clone();
        Self {
            player_id,
            uses_v3_tactics,
            map,
            registry,
            damage_chart,
            occupancy,
            properties,
            unit_costs,
            enemies,
            squads,
            action_targets,
        }
    }

    /// 敵の占有マス（座標順）。同じ配置の間は距離cacheを再利用できる。
    pub(crate) fn enemy_blocker_positions(&self) -> Vec<(usize, usize)> {
        let mut positions = self
            .enemies
            .iter()
            .map(|(position, ..)| (position.x, position.y))
            .collect::<Vec<_>>();
        positions.sort_unstable();
        positions
    }

    /// 自軍所有で、この兵種が回復できる物件。
    pub(crate) fn own_repair_sites<'a>(
        &'a self,
        stats: &'a UnitStats,
    ) -> impl Iterator<Item = GridPosition> + 'a {
        self.properties
            .iter()
            .filter(move |(_, terrain, owner)| {
                *owner == Some(self.player_id)
                    && self
                        .registry
                        .can_repair_on_terrain(stats.unit_type, *terrain)
            })
            .map(|(position, ..)| *position)
    }

    /// 自軍所有の生産施設。
    pub(crate) fn own_production_sites(&self) -> impl Iterator<Item = GridPosition> + '_ {
        self.properties
            .iter()
            .filter(|(_, terrain, owner)| {
                *owner == Some(self.player_id)
                    && self.registry.is_production_facility(terrain.as_str())
            })
            .map(|(position, ..)| *position)
    }

    /// 自軍が所有していない物件（中立・敵）。
    pub(crate) fn unowned_properties(&self) -> impl Iterator<Item = GridPosition> + '_ {
        self.properties
            .iter()
            .filter(|(_, _, owner)| *owner != Some(self.player_id))
            .map(|(position, ..)| *position)
    }

    pub(crate) fn enemy_positions(&self) -> impl Iterator<Item = GridPosition> + '_ {
        self.enemies.iter().map(|(position, ..)| *position)
    }
}

/// 輸送中の兵は輸送ユニットの価値へ畳み込み、盤外の独立標的にはしない。
fn observe_enemies(
    world: &mut World,
    player_id: PlayerId,
    properties: &[PropertyFact],
    unit_costs: &HashMap<Entity, u32>,
    registry: &MasterDataRegistry,
) -> Vec<EnemyThreatInfo> {
    world
        .query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>()
        .iter(world)
        .filter(|(_, faction, _, health, _, transporting)| {
            faction.0 != player_id && health.current > 0 && transporting.is_none()
        })
        .map(|(position, faction, stats, health, cargo, _)| {
            (
                *position,
                stats.unit_type,
                strategic_target_value(
                    stats, *position, faction.0, cargo, unit_costs, properties, registry,
                ),
                health.current,
                stats.min_range,
                stats.max_range,
                stats.max_movement,
            )
        })
        .collect()
}
