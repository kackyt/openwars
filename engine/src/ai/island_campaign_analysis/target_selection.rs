//! 局地作戦の物件目標と前線起点の選定。

use super::{
    ExistingCampaignOperation, GridPosition, HashSet, Island, IslandCampaignDecision, Map,
    MasterDataRegistry, MovementType, PlayerId, PropertySnapshot, Terrain, UnitSnapshot,
    capture_front_priority, get_valid_movement_cost,
};
#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_target_position(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    decision: IslandCampaignDecision,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Option<GridPosition> {
    let origins = capture_front_origins(island, properties, units, player_id, existing_operation);
    let mut targets = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect::<Vec<_>>();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_unstable_by_key(|position| target_priority(*position));
    let nearest = targets.first().copied();

    // 防衛作戦のアンカーは、奪取対象ではなく守るべき自軍施設である。敵が航空機だけで
    // 島外から接近している場合でも、候補自体を失うとThreatened島の防衛編成が作れない。
    if decision == IslandCampaignDecision::Defend {
        let mut defensive_targets = properties
            .iter()
            .filter(|snapshot| island.tiles.contains(&snapshot.position))
            .filter(|snapshot| {
                snapshot.property.max_capture_points > 0
                    && snapshot.property.owner_id == Some(player_id)
            })
            .map(|snapshot| snapshot.position)
            .collect::<Vec<_>>();
        defensive_targets.sort_unstable_by_key(|position| target_priority(*position));
        defensive_targets.dedup();
        if let Some(operation) = existing_operation
            && defensive_targets.contains(&operation.target_position)
        {
            return Some(operation.target_position);
        }
        if let Some(target) = defensive_targets.first().copied() {
            return Some(target);
        }
    }

    // 進行中目標も無条件には固定しない。より近い前線より2手以上遠くなった目標は
    // 敵の再占領や担当損耗で前提が崩れたものとして、現在の最短前線へ戻す。
    if let (Some(operation), Some(nearest)) = (existing_operation, nearest)
        && targets.contains(&operation.target_position)
    {
        let distance_from_front = |position: GridPosition| target_priority(position).1;
        if origins.is_empty()
            || distance_from_front(operation.target_position)
                <= distance_from_front(nearest).saturating_add(2)
        {
            return Some(operation.target_position);
        }
    }
    // 未所有の施設がなくなった島へ、任意の島タイルを輸送目標としてはいけない。
    // とくに島の先頭タイルが浅瀬だと、揚陸艇はcargoを降ろせない浅瀬まで進んで
    // Drop候補を失う。継続する必要があるのは、実際に残る地上敵への対処だけである。
    let mut ground_enemy_targets = units
        .iter()
        .filter(|unit| unit.faction != player_id && unit.health.current > 0)
        .filter(|unit| unit.transporting.is_none())
        .filter(|unit| island.tiles.contains(&unit.position))
        .filter(|unit| {
            !matches!(
                unit.stats.movement_type,
                MovementType::Air | MovementType::Ship
            )
        })
        // 輸送cargoが上陸できない浅瀬などを、敵の位置だけを理由に目的地へしない。
        .filter(|unit| {
            map.get_terrain(unit.position.x, unit.position.y)
                .and_then(|terrain| {
                    get_valid_movement_cost(registry, MovementType::Infantry, terrain)
                })
                .is_some()
        })
        .map(|unit| unit.position)
        .collect::<Vec<_>>();
    ground_enemy_targets.sort_unstable_by_key(|position| target_priority(*position));
    ground_enemy_targets.dedup();

    nearest.or_else(|| {
        // 進行中の作戦も、現在の地上敵を指している場合だけ継続する。既に消えた
        // 施設や任意タイルを保持すると、輸送隊だけが残るゾンビ作戦になる。
        existing_operation
            .filter(|operation| ground_enemy_targets.contains(&operation.target_position))
            .map(|operation| operation.target_position)
            .or_else(|| ground_enemy_targets.first().copied())
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_capture_target_positions(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    primary_target: GridPosition,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Vec<GridPosition> {
    let origins = capture_front_origins(island, properties, units, player_id, existing_operation);
    let mut targets: Vec<_> = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_by_key(|position| target_priority(*position));

    let own_capital = properties
        .iter()
        .find(|snapshot| {
            island.tiles.contains(&snapshot.position)
                && snapshot.property.terrain == Terrain::Capital
                && snapshot.property.owner_id == Some(player_id)
        })
        .map(|snapshot| snapshot.position);
    let enemy_capital = properties
        .iter()
        .find(|snapshot| {
            island.tiles.contains(&snapshot.position)
                && snapshot.property.terrain == Terrain::Capital
                && snapshot
                    .property
                    .owner_id
                    .is_some_and(|owner| owner != player_id)
        })
        .map(|snapshot| snapshot.position);

    let route_fronts = if let (Some(own_cap), Some(enemy_cap)) = (own_capital, enemy_capital) {
        crate::ai::v4::capital_route::topology::armored_capital_route_fronts(
            map, registry, own_cap, enemy_cap,
        )
    } else {
        Vec::new()
    };

    // 既に足場がある陸塊では、最短前線とその隣接帯だけを同時実行対象にする。
    // 全未所有施設は毎手番再観測するため捨てず、前線取得後に次の帯へ昇格する。
    // 足場のない島は輸送波の目的目的一覧を縮めると再上陸が遅れるため従来どおり保持する。
    if !origins.is_empty() {
        if route_fronts.len() >= 2 {
            // 同一陸塊で複数の進軍軸（上段・下段等）がある場合、各ルートの最寄り拠点を
            // 独立に保持する。単一前線で一括切断すると、片方のルートの占領が消滅する。
            let nearest_route = |position: GridPosition| {
                route_fronts
                    .iter()
                    .enumerate()
                    .min_by_key(|(route, front)| {
                        (
                            map.distance(position.x, position.y, front.x, front.y),
                            *route,
                        )
                    })
                    .map_or(0, |(route, _)| route)
            };
            let mut min_distance_by_route: crate::collections::HashMap<usize, u32> =
                crate::collections::HashMap::default();
            for target in &targets {
                let route = nearest_route(*target);
                let distance = target_priority(*target).1;
                min_distance_by_route
                    .entry(route)
                    .and_modify(|min| *min = (*min).min(distance))
                    .or_insert(distance);
            }
            targets.retain(|target| {
                let route = nearest_route(*target);
                let distance = target_priority(*target).1;
                let min_distance = min_distance_by_route.get(&route).copied().unwrap_or(0);
                distance <= min_distance.saturating_add(2)
            });
        } else if let Some(nearest_distance) =
            targets.first().map(|target| target_priority(*target).1)
        {
            targets
                .retain(|target| target_priority(*target).1 <= nearest_distance.saturating_add(2));
        }
    }
    if let Some(index) = targets
        .iter()
        .position(|position| *position == primary_target)
    {
        targets.remove(index);
        targets.insert(0, primary_target);
    }
    targets
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_front_origins(
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Vec<GridPosition> {
    let owned_property_positions = || {
        properties
            .iter()
            .filter(|snapshot| island.tiles.contains(&snapshot.position))
            .filter(|snapshot| snapshot.property.owner_id == Some(player_id))
            .map(|snapshot| snapshot.position)
            .collect::<Vec<_>>()
    };
    let has_own_capital = properties.iter().any(|snapshot| {
        island.tiles.contains(&snapshot.position)
            && snapshot.property.terrain == Terrain::Capital
            && snapshot.property.owner_id == Some(player_id)
    });
    let has_enemy_capital = properties.iter().any(|snapshot| {
        island.tiles.contains(&snapshot.position)
            && snapshot.property.terrain == Terrain::Capital
            && snapshot
                .property
                .owner_id
                .is_some_and(|owner| owner != player_id)
    });
    if has_own_capital && has_enemy_capital {
        // 同一陸塊の首都戦役は、担当歩兵一体の位置で戦略前線を後退させない。
        // 所有施設群を補給線として測るmainの挙動を維持し、局地CaptureはSquad order、
        // 首都方向の継続目標はCampaignがそれぞれ担当する。
        return owned_property_positions();
    }
    let live_capture_position = |unit: &&UnitSnapshot| {
        unit.faction == player_id
            && unit.stats.can_capture
            && unit.transporting.is_none()
            && island.tiles.contains(&unit.position)
    };
    if let Some(operation) = existing_operation {
        let assigned_entities = operation
            .capture_entities
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let assigned_positions = units
            .iter()
            .filter(live_capture_position)
            .filter(|unit| assigned_entities.contains(&unit.entity))
            .map(|unit| unit.position)
            .collect::<Vec<_>>();
        if !assigned_positions.is_empty() {
            return assigned_positions;
        }
    }
    let available_positions = units
        .iter()
        .filter(live_capture_position)
        .map(|unit| unit.position)
        .collect::<Vec<_>>();
    if !available_positions.is_empty() {
        return available_positions;
    }
    owned_property_positions()
}
