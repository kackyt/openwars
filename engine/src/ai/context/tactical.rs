//! 戦術・生産評価が参照する読み取り専用の盤面スナップショットと標的価値計算。
use crate::collections::{HashMap, HashSet};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::resources::master_data::{MasterDataRegistry, UnitName, WeaponRecord};
use crate::resources::{GridTopology, Terrain, UnitType};
use bevy_ecs::prelude::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TacticalSnapshotUnit {
    pub(crate) entity: Entity,
    pub(crate) position: GridPosition,
    pub(crate) faction: PlayerId,
    unit_type: UnitType,
    pub(crate) ammo1: u32,
    pub(crate) ammo2: u32,
    pub(crate) is_transporting: bool,
}

#[derive(Debug, Clone, Copy)]
struct TacticalSnapshotProperty {
    owner: Option<PlayerId>,
    capture_points: u32,
    max_capture_points: u32,
}

/// 1回の行動選択中に不変な盤面情報を索引化し、移動候補ごとの ECS 全走査を避けます。
pub(crate) struct AiTacticalSnapshot {
    topology: GridTopology,
    units: Vec<TacticalSnapshotUnit>,
    unit_indices: HashMap<Entity, usize>,
    units_by_position: HashMap<(usize, usize), Vec<usize>>,
    properties: HashMap<(usize, usize), TacticalSnapshotProperty>,
    weapons: HashMap<UnitType, (Option<WeaponRecord>, Option<WeaponRecord>)>,
}

impl AiTacticalSnapshot {
    /// snapshotのunit一覧を読み取り専用で渡す。
    pub(crate) fn units(&self) -> &[TacticalSnapshotUnit] {
        &self.units
    }

    pub(crate) fn from_world(
        world: &mut World,
        registry: &MasterDataRegistry,
        topology: GridTopology,
    ) -> Self {
        let mut units = Vec::new();
        let mut unit_indices = HashMap::default();
        let mut units_by_position: HashMap<(usize, usize), Vec<usize>> = HashMap::default();
        {
            let mut query = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                Option<&crate::components::Ammo>,
                Option<&crate::components::Transporting>,
            )>();
            for (entity, position, faction, stats, ammo, transporting) in query.iter(world) {
                let index = units.len();
                units.push(TacticalSnapshotUnit {
                    entity,
                    position: *position,
                    faction: faction.0,
                    unit_type: stats.unit_type,
                    ammo1: ammo.map_or(0, |value| value.ammo1),
                    ammo2: ammo.map_or(0, |value| value.ammo2),
                    is_transporting: transporting.is_some(),
                });
                unit_indices.insert(entity, index);
                units_by_position
                    .entry((position.x, position.y))
                    .or_default()
                    .push(index);
            }
        }

        let properties = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(world)
                .map(|(position, property)| {
                    (
                        (position.x, position.y),
                        TacticalSnapshotProperty {
                            owner: property.owner_id,
                            capture_points: property.capture_points,
                            max_capture_points: property.max_capture_points,
                        },
                    )
                })
                .collect()
        };

        let weapons = units
            .iter()
            .map(|unit| unit.unit_type)
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|unit_type| {
                let unit_record = registry.get_unit(&UnitName(unit_type.as_str().to_string()));
                let pair = unit_record.map_or((None, None), |record| {
                    let primary = record
                        .weapon1
                        .as_ref()
                        .and_then(|name| registry.weapons.get(&UnitName(name.clone())))
                        .cloned();
                    let secondary = record
                        .weapon2
                        .as_ref()
                        .and_then(|name| registry.weapons.get(&UnitName(name.clone())))
                        .cloned();
                    (primary, secondary)
                });
                (unit_type, pair)
            })
            .collect();

        Self {
            topology,
            units,
            unit_indices,
            units_by_position,
            properties,
            weapons,
        }
    }

    pub(crate) fn action_targets_at(
        &self,
        attacker: Entity,
        stats: &UnitStats,
        position: GridPosition,
        is_moved: bool,
    ) -> crate::systems::action::TacticalActionTargets {
        self.action_targets_at_observed(attacker, stats, position, is_moved, None)
    }

    /// 診断は実際に通過した判定だけを記録する。通常口もこの同じ経路を使用する。
    pub(crate) fn action_targets_at_observed(
        &self,
        attacker: Entity,
        stats: &UnitStats,
        position: GridPosition,
        is_moved: bool,
        mut probe: Option<&mut crate::ai::wait_probe::UnitProbe>,
    ) -> crate::systems::action::TacticalActionTargets {
        let Some(&attacker_index) = self.unit_indices.get(&attacker) else {
            if let Some(p) = probe.as_deref_mut() {
                p.reject("actor_not_in_snapshot", position, || serde_json::json!({}));
            }
            return crate::systems::action::TacticalActionTargets::default();
        };
        let attacker_unit = self.units[attacker_index];
        let position_indices = self
            .units_by_position
            .get(&(position.x, position.y))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let is_occupied_by_other = position_indices.iter().any(|&index| {
            let unit = self.units[index];
            unit.entity != attacker && !unit.is_transporting
        });

        let mergeable_targets = position_indices
            .iter()
            .filter_map(|&index| {
                let target = self.units[index];
                (target.entity != attacker
                    && target.faction == attacker_unit.faction
                    && target.unit_type == stats.unit_type)
                    .then_some(target.entity)
            })
            .collect();

        let (can_capture, can_repair) = self
            .properties
            .get(&(position.x, position.y))
            .filter(|property| stats.can_capture && property.max_capture_points > 0)
            .map_or((false, false), |property| {
                if property.owner == Some(attacker_unit.faction) {
                    (false, property.capture_points < property.max_capture_points)
                } else {
                    (true, false)
                }
            });

        if let Some(p) = probe.as_deref_mut() {
            if is_occupied_by_other {
                p.reject("tile_occupied_by_other", position, || serde_json::json!({
                    "occupants":position_indices.iter().filter_map(|&i| {
                        let u=self.units[i]; (!u.is_transporting && u.entity!=attacker).then_some(u.entity.to_bits())
                    }).collect::<Vec<_>>(),"attack_generation_skipped":true
                }));
            }
            if !can_capture || is_occupied_by_other {
                p.reject("capture_eligibility_false", position, || serde_json::json!({
                    "can_capture":stats.can_capture,"has_property":self.properties.contains_key(&(position.x,position.y)),
                    "owner":self.properties.get(&(position.x,position.y)).and_then(|p|p.owner).map(|p|p.0),
                    "occupied_by_other":is_occupied_by_other,
                    "max_capture_points":self.properties.get(&(position.x,position.y)).map(|p|p.max_capture_points)
                }));
            }
        }
        let attackable_targets = if is_occupied_by_other {
            Vec::new()
        } else {
            let (primary, secondary) = self
                .weapons
                .get(&stats.unit_type)
                .map(|(primary, secondary)| (primary.as_ref(), secondary.as_ref()))
                .unwrap_or((None, None));
            self.units
                .iter()
                .filter_map(|target| {
                    if target.entity == attacker || target.faction == attacker_unit.faction {
                        return None;
                    }
                    let distance = self.topology.distance(
                        (position.x, position.y),
                        (target.position.x, target.position.y),
                    );
                    let mut weapon_can_attack = |weapon: &WeaponRecord, ammo: u32, slot: &str| {
                        // 元のANDの順序を維持し、最初に実際に落ちた条件だけを記録する。
                        let reason = if weapon.damages.get(target.unit_type.as_str()).copied().unwrap_or(0) == 0 {
                            Some("attack_weapon_zero_damage")
                        } else if ammo == 0 {
                            Some("attack_weapon_no_ammo")
                        } else if distance > 1 && is_moved {
                            Some("attack_ranged_after_move")
                        } else if distance < weapon.range_min {
                            Some("attack_inside_min_range")
                        } else if distance > weapon.range_max {
                            Some("attack_outside_max_range")
                        } else { None };
                        if let Some(reason) = reason {
                            if let Some(p) = probe.as_deref_mut() {
                                p.reject(reason, position, || serde_json::json!({"target_id":target.entity.to_bits(),
                                    "weapon_slot":slot,"distance":distance,"ammo":ammo,"is_moved":is_moved,
                                    "min_range":weapon.range_min,"max_range":weapon.range_max}));
                            }
                            false
                        } else { true }
                    };
                    let allowed = primary.is_some_and(|weapon| weapon_can_attack(weapon, attacker_unit.ammo1, "primary"))
                        || secondary.is_some_and(|weapon| weapon_can_attack(weapon, attacker_unit.ammo2, "secondary"));
                    if !allowed && let Some(p) = probe.as_deref_mut() {
                        p.reject("attack_target_no_usable_weapon", position, || serde_json::json!({
                            "target_id":target.entity.to_bits(),"primary_exists":primary.is_some(),"secondary_exists":secondary.is_some()}));
                    }
                    allowed.then_some(target.entity)
                })
                .collect()
        };

        crate::systems::action::TacticalActionTargets {
            attackable_targets,
            mergeable_targets,
            can_capture: !is_occupied_by_other && can_capture,
            can_repair: !is_occupied_by_other && can_repair,
            can_wait: !is_occupied_by_other || !is_moved,
        }
    }
}

pub(crate) fn transport_has_other_actionable_cargo(
    world: &World,
    transport: Entity,
    current_cargo: Entity,
) -> bool {
    world
        .get::<crate::components::CargoCapacity>(transport)
        .is_some_and(|capacity| {
            capacity.loaded.iter().any(|loaded| {
                *loaded != current_cargo
                    && world
                        .get::<crate::components::ActionCompleted>(*loaded)
                        .is_some_and(|action| !action.0)
            })
        })
}

/// 標的本体だけでなく、輸送中の兵力と目前の占領による収入損失も含めた戦略価値を返す。
/// 輸送ユニットへのダメージは搭載ユニットにも同期されるため、搭載兵の価格を同率で評価する。
pub(crate) fn strategic_target_value(
    stats: &UnitStats,
    position: GridPosition,
    owner: PlayerId,
    cargo: Option<&crate::components::CargoCapacity>,
    unit_costs: &HashMap<Entity, u32>,
    properties: &[(GridPosition, Terrain, Option<PlayerId>)],
    registry: &MasterDataRegistry,
) -> u32 {
    let cargo_value = cargo
        .into_iter()
        .flat_map(|capacity| capacity.loaded.iter())
        .filter_map(|entity| unit_costs.get(entity))
        .fold(0_u32, |total, cost| total.saturating_add(*cost));

    // 占領可能ユニットが他勢力の物件上にいれば、次に失い得る1ターン分の収入を加える。
    let capture_risk = if stats.can_capture {
        properties
            .iter()
            .find(|(property_position, _, property_owner)| {
                *property_position == position && *property_owner != Some(owner)
            })
            .map(|(_, terrain, _)| registry.landscape_income(terrain.as_str()))
            .unwrap_or(0)
    } else {
        0
    };

    stats
        .cost
        .saturating_add(cargo_value)
        .saturating_add(capture_risk)
}

pub(crate) fn has_capturable_property_at(
    world: &World,
    position: GridPosition,
    player_id: PlayerId,
) -> bool {
    world.iter_entities().any(|entity_ref| {
        entity_ref.get::<GridPosition>().copied() == Some(position)
            && entity_ref.get::<Property>().is_some_and(|property| {
                property.max_capture_points > 0 && property.owner_id != Some(player_id)
            })
    })
}

pub(crate) fn is_unit_stranded(
    world: &World,
    pos: &GridPosition,
    player_id: PlayerId,
    properties: &[(GridPosition, crate::resources::Terrain, Option<PlayerId>)],
    enemy_units: &[(
        GridPosition,
        crate::resources::UnitType,
        u32,
        u32,
        u32,
        u32,
        u32,
    )],
) -> bool {
    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
        && let Some(my_island) = island_map.get_island_at(pos)
    {
        let mut local_targets = false;
        for (p_pos, _, p_owner) in properties {
            if *p_owner != Some(player_id) && my_island.tiles.contains(p_pos) {
                local_targets = true;
                break;
            }
        }

        let mut local_enemies = false;
        for (e_pos, _, _, _, _, _, _) in enemy_units {
            if my_island.tiles.contains(e_pos) {
                local_enemies = true;
                break;
            }
        }

        if !local_targets && !local_enemies {
            return true;
        }
    }
    false
}
