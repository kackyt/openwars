//! RollingPlanのrole_multiset責務。
use super::frontier::{first_differing_frontier_field, multiset_frontier_key};
use super::{GridPosition, HashMap, MovementType, RollingPlanInput, UnitType, best_damage};

/// 物件レースにおける抽象生産要求（役割）。
/// 施設ごとの順列展開を排除し、マルチセット（何を何体作るか）として組合せを表現する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ProductionRole {
    Capture {
        target: GridPosition,
        unit_type: UnitType,
        cost: u32,
    },
    Combat {
        unit_type: UnitType,
        cost: u32,
    },
}

impl ProductionRole {
    pub(super) fn cost(&self) -> u32 {
        match *self {
            ProductionRole::Capture { cost, .. } | ProductionRole::Combat { cost, .. } => cost,
        }
    }

    pub(super) fn capture_target(&self) -> Option<GridPosition> {
        match *self {
            ProductionRole::Capture { target, .. } => Some(target),
            ProductionRole::Combat { .. } => None,
        }
    }

    pub(super) fn unit_type(&self) -> UnitType {
        match *self {
            ProductionRole::Capture { unit_type, .. }
            | ProductionRole::Combat { unit_type, .. } => unit_type,
        }
    }
}

/// 多工場の物件レースにおいて各サイズ段階で保持するマルチセット候補のビーム幅。
/// 通常マップ（施設数5以下）では各段階の候補数がこの上限に達しないため、全候補が保持される。
/// 施設数が多い盤面での順列・組合せ爆発を防ぎつつ、最も有望な編成を保持する。
pub(super) const MULTISET_BEAM_WIDTH: usize = 384;

/// 同一の戦闘ユニット役割を同一手番に重複生産する上限。
/// 前線物件制御において同種戦闘ユニット（戦車・対空等）の極端な偏重を抑え、諸兵科連合を促す。
pub(super) const MAX_COMBAT_ROLE_COPIES: usize = 3;

#[derive(Debug, Clone)]
pub(super) struct RoleMetadata {
    pub(super) cost: u32,
    pub(super) capture_target: Option<GridPosition>,
    pub(super) capacity: usize,
    pub(super) min_capture_completion: u32,
    pub(super) is_combat_non_infantry: bool,
    pub(super) max_movement: u32,
    pub(super) damage_by_enemy: Vec<u32>,
}

#[derive(Clone)]
pub(super) struct MultisetCandidate {
    pub(super) role_indices: Vec<usize>,
    /// 占領対象数は高々数個〜10個程度（MAX_CAPTURE_SLOTS=8）に収まるため、
    /// HashSetのヒープ確保・ハッシュ計算オーバーヘッドを避け、連続メモリのVecによる線形探索（L1キャッシュ効率優先）を採用。
    pub(super) used_targets: Vec<GridPosition>,
    pub(super) current_role_count: usize,
    pub(super) remaining_funds: u32,
    pub(super) last_role_idx: usize,
}

/// 物件制御における候補（マルチセットおよびSearchState）の順序付けキー。
/// 評価軸の一貫性を保ち、multiset探索段階と最終フロンティア段階での評価乖離を防ぐ。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PropertyFrontierKey {
    /// 占領必要数に対する不足数（0が最良）
    pub(super) capture_shortfall: usize,
    /// 小規模戦術ルールにおける占領完了予定ターン（昇順）
    pub(super) small_map_capture_completion: Vec<u32>,
    /// 小規模戦術ルールにおける非歩兵戦闘ユニット数（降順）
    pub(super) combat_unit_count_desc: std::cmp::Reverse<usize>,
    /// 全ユニットの合計移動力（降順）
    pub(super) total_movement_desc: std::cmp::Reverse<u32>,
    /// 締切対象・関連敵の残HP合計（昇順）
    pub(super) remaining_relevant_hp: u32,
    /// 全敵の残HP合計（昇順）
    pub(super) remaining_all_hp: u32,
    /// 全占領予定の完了ターン（昇順、required_capturersでresize済み）
    pub(super) capture_completion_turns: Vec<u32>,
    /// 占領必要数を超える余剰占領ユニットのペナルティ（昇順）
    pub(super) excess_capturers: usize,
    /// 生産コスト合計（昇順）
    pub(super) cost: u32,
    /// 同点時の決定論的タイブレーク用インデックス列
    pub(super) identity_indices: Vec<usize>,
}

/// 役割のマルチセット（重複組合せ）を資金上限およびスロット上限の範囲で生成する。
/// 同一占領対象への二重割当は除外され、戦闘ユニットは重複が許容される。
/// 施設数が多い盤面では、サイズ段階ごとにビーム幅で有望な候補を維持し、組合せ爆発を防ぐ。
pub(super) fn generate_role_multisets(
    roles: &[ProductionRole],
    input: &RollingPlanInput,
    facilities: &[(GridPosition, Vec<usize>)],
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
    current_funds: u32,
    max_slots: usize,
) -> (Vec<Vec<ProductionRole>>, bool) {
    if roles.is_empty() || max_slots == 0 {
        return (Vec::new(), false);
    }

    let role_metas: Vec<RoleMetadata> = roles
        .iter()
        .map(|role| {
            let matching_facs: Vec<(GridPosition, usize)> = facilities
                .iter()
                .filter_map(|(fac, _)| {
                    facility_role_options
                        .get(&(*fac, *role))
                        .copied()
                        .map(|opt_idx| (*fac, opt_idx))
                })
                .collect();
            let capacity = matching_facs.len();

            let first_opt = matching_facs
                .first()
                .map(|&(_, opt_idx)| &input.production_options[opt_idx]);
            let is_combat_non_infantry = role.capture_target().is_none()
                && first_opt.is_some_and(|opt| opt.stats.movement_type != MovementType::Infantry);
            let max_movement = first_opt.map_or(0, |opt| opt.stats.max_movement);

            let min_capture_completion = if role.capture_target().is_some() {
                matching_facs
                    .iter()
                    .filter_map(|&(_, opt_idx)| {
                        input.production_options[opt_idx].capture_completion_turn
                    })
                    .min()
                    .unwrap_or(u32::MAX)
            } else {
                u32::MAX
            };

            let damage_by_enemy: Vec<u32> = input
                .enemies
                .iter()
                .enumerate()
                .map(|(enemy_idx, enemy)| {
                    if role.capture_target().is_some() {
                        return 0;
                    }
                    let can_reach = matching_facs.iter().any(|&(_, opt_idx)| {
                        input
                            .production_attack_projections
                            .get(opt_idx)
                            .map_or_else(
                                || {
                                    input.production_options[opt_idx]
                                        .engageable_enemy_indices
                                        .contains(&enemy_idx)
                                },
                                |projections| {
                                    projections.get(enemy_idx).is_some_and(Option::is_some)
                                },
                            )
                    });
                    if can_reach {
                        best_damage(&input.damage_chart, role.unit_type(), enemy.stats.unit_type)
                    } else {
                        0
                    }
                })
                .collect();

            RoleMetadata {
                cost: role.cost(),
                capture_target: role.capture_target(),
                capacity,
                min_capture_completion,
                is_combat_non_infantry,
                max_movement,
                damage_by_enemy,
            }
        })
        .collect();

    // 【一時的な診断専用】役割indexごとの「間接（min_range>1）か」を、
    // 環境変数が設定された時だけ作る。生成ロジックからは参照されない。
    let funnel_indirect_roles: Option<Vec<bool>> =
        std::env::var_os("OPENWARS_INDIRECT_FUNNEL_AUDIT")
            .is_some()
            .then(|| {
                roles
                    .iter()
                    .map(|role| {
                        facilities
                            .iter()
                            .find_map(|(facility, _)| {
                                facility_role_options.get(&(*facility, *role)).copied()
                            })
                            .is_some_and(|option_index| {
                                input.production_options[option_index].stats.min_range > 1
                            })
                    })
                    .collect()
            });

    let initial = MultisetCandidate {
        role_indices: Vec::new(),
        used_targets: Vec::new(),
        current_role_count: 0,
        remaining_funds: current_funds,
        last_role_idx: 0,
    };

    let mut current_level = vec![initial];
    let mut all_multisets = Vec::new();
    let mut truncated = false;

    for _size in 1..=max_slots {
        let mut next_level = Vec::new();
        for parent in &current_level {
            for (i, meta) in role_metas.iter().enumerate().skip(parent.last_role_idx) {
                if meta.cost > parent.remaining_funds {
                    continue;
                }

                let next_role_count = if i == parent.last_role_idx {
                    parent.current_role_count.saturating_add(1)
                } else {
                    1
                };

                if let Some(target) = meta.capture_target {
                    if parent.used_targets.contains(&target) {
                        continue;
                    }
                    let mut used_targets = parent.used_targets.clone();
                    used_targets.push(target);
                    let mut new_role_indices = parent.role_indices.clone();
                    new_role_indices.push(i);
                    next_level.push(MultisetCandidate {
                        role_indices: new_role_indices,
                        used_targets,
                        current_role_count: next_role_count,
                        remaining_funds: parent.remaining_funds.saturating_sub(meta.cost),
                        last_role_idx: i,
                    });
                } else if next_role_count <= meta.capacity
                    && next_role_count <= MAX_COMBAT_ROLE_COPIES
                {
                    let mut new_role_indices = parent.role_indices.clone();
                    new_role_indices.push(i);
                    next_level.push(MultisetCandidate {
                        role_indices: new_role_indices,
                        used_targets: parent.used_targets.clone(),
                        current_role_count: next_role_count,
                        remaining_funds: parent.remaining_funds.saturating_sub(meta.cost),
                        last_role_idx: i,
                    });
                }
            }
        }

        // 【一時的な診断専用】ビーム切り捨ての前後で間接役を含む候補が何件残るかを記録する。
        // 環境変数が無い通常対戦では一切実行されない。探索結果は変更しない。
        let next_level_snapshot = if funnel_indirect_roles.is_some() {
            next_level.clone()
        } else {
            Vec::new()
        };
        let funnel_audit_before = funnel_indirect_roles.as_ref().map(|indirect| {
            next_level
                .iter()
                .filter(|cand| cand.role_indices.iter().any(|idx| indirect[*idx]))
                .count()
        });

        if next_level.len() > MULTISET_BEAM_WIDTH {
            truncated = true;
            next_level.sort_unstable_by_key(|cand| {
                multiset_frontier_key(input, &cand.role_indices, &role_metas)
            });
            next_level.truncate(MULTISET_BEAM_WIDTH);
        }

        if let (Some(indirect), Some(before)) =
            (funnel_indirect_roles.as_ref(), funnel_audit_before)
        {
            let after = next_level
                .iter()
                .filter(|cand| cand.role_indices.iter().any(|idx| indirect[*idx]))
                .count();
            // 切り捨てで全滅した場合だけ、最良の間接候補と最下位の残存候補を突き合わせ、
            // どのキー成分で負けたかを一つだけ特定する。
            let mut detail = String::new();
            if before > 0 && after == 0 {
                let best_indirect = next_level_snapshot
                    .iter()
                    .filter(|cand| cand.role_indices.iter().any(|idx| indirect[*idx]))
                    .min_by_key(|cand| {
                        multiset_frontier_key(input, &cand.role_indices, &role_metas)
                    });
                let worst_retained = next_level.iter().max_by_key(|cand| {
                    multiset_frontier_key(input, &cand.role_indices, &role_metas)
                });
                if let (Some(rejected), Some(retained)) = (best_indirect, worst_retained) {
                    let rejected_key =
                        multiset_frontier_key(input, &rejected.role_indices, &role_metas);
                    let retained_key =
                        multiset_frontier_key(input, &retained.role_indices, &role_metas);
                    detail = format!(
                        " first_differing_key={} rejected(roles={:?} cost={} capture_shortfall={} completion={:?} combat_desc={:?} movement_desc={:?} rel_hp={} all_hp={} excess={}) retained(roles={:?} cost={} capture_shortfall={} completion={:?} combat_desc={:?} movement_desc={:?} rel_hp={} all_hp={} excess={})",
                        first_differing_frontier_field(&rejected_key, &retained_key),
                        rejected
                            .role_indices
                            .iter()
                            .map(|idx| (roles[*idx].unit_type(), roles[*idx].capture_target()))
                            .collect::<Vec<_>>(),
                        rejected_key.cost,
                        rejected_key.capture_shortfall,
                        rejected_key.capture_completion_turns,
                        rejected_key.combat_unit_count_desc,
                        rejected_key.total_movement_desc,
                        rejected_key.remaining_relevant_hp,
                        rejected_key.remaining_all_hp,
                        rejected_key.excess_capturers,
                        retained
                            .role_indices
                            .iter()
                            .map(|idx| (roles[*idx].unit_type(), roles[*idx].capture_target()))
                            .collect::<Vec<_>>(),
                        retained_key.cost,
                        retained_key.capture_shortfall,
                        retained_key.capture_completion_turns,
                        retained_key.combat_unit_count_desc,
                        retained_key.total_movement_desc,
                        retained_key.remaining_relevant_hp,
                        retained_key.remaining_all_hp,
                        retained_key.excess_capturers,
                    );
                }
            }
            eprintln!(
                "INDIRECT_FUNNEL_AUDIT stage=multiset_beam turn={} size={} generated={} beam_width={} indirect_before={} indirect_after={} truncated={}{}",
                input.current_turn,
                _size,
                next_level_snapshot.len(),
                MULTISET_BEAM_WIDTH,
                before,
                after,
                next_level_snapshot.len() > MULTISET_BEAM_WIDTH,
                detail,
            );
        }

        for cand in &next_level {
            all_multisets.push(cand.role_indices.iter().map(|&idx| roles[idx]).collect());
        }

        current_level = next_level;
        if current_level.is_empty() {
            break;
        }
    }

    (all_multisets, truncated)
}
