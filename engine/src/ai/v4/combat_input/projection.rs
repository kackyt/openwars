//! 手番内の生産判断に必要な補助処理。
use crate::ai::v4::property_control::{player_order, produced_attack_turn_along_capture_route};
use crate::ai::v4::*;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

pub(in crate::ai::v4) fn next_activation_occupants(
    scan: &BoardScan,
    operation: &Operation,
    player_id: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    let opposing_player = player_id.opposite();
    scan.my_units
        .iter()
        .filter(|unit| {
            unit.entity
                .is_some_and(|entity| operation.protected_capture_entities.contains(&entity))
        })
        .map(|unit| (unit, player_id))
        .chain(scan.enemy_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}

pub(in crate::ai::v4) fn projected_enemy_front_cells(
    scan: &BoardScan,
    operation: &Operation,
    enemy: &UnitSnapshot,
    player_id: PlayerId,
) -> Vec<GridPosition> {
    let enemy_player = player_id.opposite();
    if player_order(player_id) <= player_order(enemy_player)
        || enemy.stats.max_movement == 0
        || enemy.stats.min_range > 1
    {
        return vec![enemy.pos];
    }

    let occupancy = scanned_occupants_for_mover(scan, enemy_player, player_id);
    let fuel = if enemy.stats.max_fuel == 0 {
        u32::MAX
    } else {
        enemy.stats.max_fuel
    };
    let reachable = calculate_reachable_tile_costs(
        &scan.map,
        &occupancy,
        (enemy.pos.x, enemy.pos.y),
        enemy.stats.movement_type,
        enemy.stats.max_movement,
        fuel,
        enemy_player,
        enemy.stats.unit_type,
        &scan.master_data,
    );
    let mut best_distance = u32::MAX;
    let mut cells = Vec::new();
    for ((x, y), _) in reachable {
        // 合流先は元Entityが消える別行動なので、単体敵の位置投影には使わない。
        if (x, y) != (enemy.pos.x, enemy.pos.y) && occupancy.contains_key(&(x, y)) {
            continue;
        }
        let distance = scan
            .map
            .distance(x, y, operation.anchor.x, operation.anchor.y);
        match distance.cmp(&best_distance) {
            std::cmp::Ordering::Less => {
                best_distance = distance;
                cells.clear();
                cells.push(GridPosition { x, y });
            }
            std::cmp::Ordering::Equal => cells.push(GridPosition { x, y }),
            std::cmp::Ordering::Greater => {}
        }
    }
    if cells.is_empty() {
        vec![enemy.pos]
    } else {
        cells.sort_unstable_by_key(|cell| (cell.y, cell.x));
        cells
    }
}

pub(in crate::ai::v4) fn scanned_occupants_for_mover(
    scan: &BoardScan,
    moving_player: PlayerId,
    opposing_player: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    scan.enemy_units
        .iter()
        .map(|unit| (unit, moving_player))
        .chain(scan.my_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}

use super::{CombatDraft, CombatInputs};
pub(super) fn project_production_attacks(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    draft: &CombatDraft,
) -> Vec<Vec<Option<ProductionAttackProjection>>> {
    let CombatInputs {
        scan,
        op,
        player_id,
        ..
    } = *input;
    let enemies = &draft.enemies;
    let options = &draft.options;
    let next_occupancy = next_activation_occupants(scan, op, player_id);
    let projected_target_cells = enemies
        .iter()
        .map(|enemy| {
            scan.enemy_units
                .iter()
                .find(|unit| unit.entity == enemy.entity)
                .map_or_else(
                    || vec![enemy.position],
                    |observed| projected_enemy_front_cells(scan, op, observed, player_id),
                )
        })
        .collect::<Vec<_>>();
    let projected_occupancies = enemies
        .iter()
        .map(|enemy| {
            let mut occupancy = next_occupancy.clone();
            if scan
                .enemy_units
                .iter()
                .any(|unit| unit.entity == enemy.entity)
            {
                // 対象自身の現在セルは、一手先包絡へ進んだ後の経路障害にしない。
                occupancy.remove(&(enemy.position.x, enemy.position.y));
            }
            occupancy
        })
        .collect::<Vec<_>>();
    // occupancyは敵ごとに異なるため、占有をkeyに持たない既存cacheを共有しない。
    let mut projected_action_caches = (0..enemies.len())
        .map(|_| ActionTurnDistanceCache::default())
        .collect::<Vec<_>>();

    let scene = ProjectionScene {
        scan,
        op,
        player_id,
        targets: &projected_target_cells,
        occupancies: &projected_occupancies,
    };
    options
        .iter()
        .map(|option| {
            enemies
                .iter()
                .enumerate()
                .map(|(enemy_index, enemy)| {
                    project_option_attack(
                        &scene,
                        ctx,
                        option,
                        enemy,
                        enemy_index,
                        &mut projected_action_caches[enemy_index],
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
}
/// 敵ごとに異なる一手先の占有と到達候補。cacheは敵単位に別途借用する。
struct ProjectionScene<'a> {
    scan: &'a BoardScan,
    op: &'a Operation,
    player_id: PlayerId,
    targets: &'a [Vec<GridPosition>],
    occupancies: &'a [HashMap<(usize, usize), OccupantInfo>],
}

fn project_option_attack(
    scene: &ProjectionScene<'_>,
    ctx: &mut ReachCtx,
    option: &rolling_plan::ProductionPlanOption,
    enemy: &EnemyPlanUnit,
    enemy_index: usize,
    action_cache: &mut ActionTurnDistanceCache,
) -> Option<ProductionAttackProjection> {
    let ProjectionScene { scan, op, .. } = *scene;
    if best_damage(
        &scan.damage_chart,
        option.stats.unit_type,
        enemy.stats.unit_type,
    ) == 0
    {
        return None;
    }
    let has_only_one_move_of_fuel =
        option.stats.min_range > 1 && option.stats.max_fuel > 0 && option.stats.max_fuel <= 1;
    let has_spare_production_capacity = scan.free_facilities.len() > 1;
    let capital_side_defense = op.kind == OperationKind::Defense
        && scan.capital_position.is_some_and(|capital| {
            scan.map
                .distance(capital.x, capital.y, op.anchor.x, op.anchor.y)
                <= option.stats.max_range
        });
    if has_only_one_move_of_fuel && !capital_side_defense && !has_spare_production_capacity {
        // 燃料が一マス分しかない砲台は、前線へ展開しても再配置できない。
        // 首都側の防衛でない限り、唯一の生産口を長期占有する案は候補に
        // しない。兵種名や攻撃力ではなく、マスターデータ上の燃料と実際の
        // 空き生産slotだけから判定する。
        return None;
    }
    if let std::ops::ControlFlow::Break(projection) =
        project_capture_route_attack(scene, ctx, option, enemy, enemy_index)
    {
        return projection;
    }
    project_range_attack(scene, option, enemy_index, action_cache)
}

fn project_capture_route_attack(
    scene: &ProjectionScene<'_>,
    ctx: &mut ReachCtx,
    option: &rolling_plan::ProductionPlanOption,
    enemy: &EnemyPlanUnit,
    enemy_index: usize,
) -> std::ops::ControlFlow<Option<ProductionAttackProjection>> {
    let ProjectionScene {
        scan,
        op,
        player_id,
        ..
    } = *scene;
    if option.stats.min_range <= 1
        && let Some(capture_target) = option.capture_target
    {
        if option.capture_blocking_enemy_index != Some(enemy_index) {
            return std::ops::ControlFlow::Break(None);
        }
        let control = op.property_controls.iter().find(|control| {
            control.property == capture_target && enemy.entity == Some(control.enemy_entity)
        });
        let Some(control) = control else {
            return std::ops::ControlFlow::Break(None);
        };
        let observed_enemy = scan
            .enemy_units
            .iter()
            .find(|unit| unit.entity == enemy.entity);
        let Some(observed_enemy) = observed_enemy else {
            return std::ops::ControlFlow::Break(None);
        };
        // 占領担当を戦闘用cloneへ分けず、同じ物件へ進む敵との経路上で
        // 合法に交戦できる場合だけ初撃edgeを与える。
        return std::ops::ControlFlow::Break(produced_attack_turn_along_capture_route(
            scan,
            ctx,
            &option.purchase.facility,
            &option.stats,
            observed_enemy,
            &capture_target,
            control.enemy_arrival_turn,
            player_id,
        ));
    }
    if option.stats.min_range <= 1
        && let Some(control) = op
            .property_controls
            .iter()
            .find(|control| enemy.entity == Some(control.enemy_entity))
        && let Some(observed_enemy) = scan
            .enemy_units
            .iter()
            .find(|unit| unit.entity == Some(control.enemy_entity))
    {
        // 中立へのInterdictだけでなく、敵所有前線のRecaptureでも敵を
        // 初期位置へ固定しない。敵の実最短経路上で接触できる快速screenを
        // 評価し、経路上に合法な交戦点が無い場合だけ現在位置を追う。
        if let Some(projection) = produced_attack_turn_along_capture_route(
            scan,
            ctx,
            &option.purchase.facility,
            &option.stats,
            observed_enemy,
            &control.property,
            control.enemy_arrival_turn,
            player_id,
        ) {
            return std::ops::ControlFlow::Break(Some(projection));
        }
    }
    std::ops::ControlFlow::Continue(())
}

fn project_range_attack(
    scene: &ProjectionScene<'_>,
    option: &rolling_plan::ProductionPlanOption,
    enemy_index: usize,
    action_cache: &mut ActionTurnDistanceCache,
) -> Option<ProductionAttackProjection> {
    let ProjectionScene {
        scan, player_id, ..
    } = *scene;
    let projected_target_cells = scene.targets;
    let projected_occupancies = scene.occupancies;
    let effective_fuel = if option.stats.max_fuel == 0 {
        u32::MAX
    } else {
        option.stats.max_fuel
    };
    projected_target_cells[enemy_index]
        .iter()
        .filter_map(|target| {
            let distance = if option.stats.min_range > 1 {
                // 間接unitは生産口の上から即射撃できても、その一手で次手番の
                // 生産枠を失う。退避してから射撃可能になるETAだけを戦力として
                // 数え、砲台を含めた高火力の見かけ上の即応性を使わない。
                calculate_action_distance_to_range_after_leaving_start(
                    &scan.map,
                    &scan.master_data,
                    &projected_occupancies[enemy_index],
                    (option.purchase.facility.x, option.purchase.facility.y),
                    (target.x, target.y),
                    option.stats.movement_type,
                    option.stats.max_movement,
                    effective_fuel,
                    option.stats.min_range,
                    option.stats.max_range,
                    player_id,
                    action_cache,
                )?
            } else {
                calculate_action_distance_to_range(
                    &scan.map,
                    &scan.master_data,
                    &projected_occupancies[enemy_index],
                    (option.purchase.facility.x, option.purchase.facility.y),
                    (target.x, target.y),
                    option.stats.movement_type,
                    option.stats.max_movement,
                    effective_fuel,
                    option.stats.min_range,
                    option.stats.max_range,
                    player_id,
                    action_cache,
                )?
            };
            Some((
                property_control::produced_action_offset(
                    option.purchase.build_turn,
                    distance.turns,
                ),
                distance,
            ))
        })
        .min_by_key(|(ready_turn, distance)| {
            (
                *ready_turn,
                distance.used_fuel,
                distance.used_mp,
                distance.firing_position.y,
                distance.firing_position.x,
            )
        })
        .map(|(ready_turn, distance)| ProductionAttackProjection {
            ready_turn,
            firing_position: distance.firing_position,
            requires_movement: distance.requires_movement,
        })
}
