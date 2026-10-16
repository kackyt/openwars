//! RollingPlanのrole_multiset責務。
use super::frontier::multiset_frontier_key;
use super::search_diagnostics::MultisetBeamAudit;
use super::{GridPosition, HashMap, MovementType, RollingPlanInput, UnitType, best_damage};

#[cfg(test)]
mod tests;

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
        .map(|role| RoleMetadata::observe(*role, input, facilities, facility_role_options))
        .collect();
    let beam_audit = MultisetBeamAudit::from_env(roles, input, facilities, facility_role_options);

    let mut current_level = vec![MultisetCandidate::empty(current_funds)];
    let mut all_multisets = Vec::new();
    let mut truncated = false;
    for size in 1..=max_slots {
        let mut next_level = expand_level(&current_level, &role_metas);
        // 【一時的な診断専用】縮約前の候補を控える。環境変数が無い通常対戦では何もしない。
        let generated = beam_audit
            .as_ref()
            .map(|audit| audit.observe_generated(&next_level));
        truncated |= retain_multiset_beam(input, &mut next_level, &role_metas);
        if let (Some(audit), Some(generated)) = (&beam_audit, generated) {
            audit.report(input, roles, &role_metas, size, generated, &next_level);
        }
        all_multisets.extend(next_level.iter().map(|candidate| candidate.roles(roles)));
        current_level = next_level;
        if current_level.is_empty() {
            break;
        }
    }
    (all_multisets, truncated)
}

impl RoleMetadata {
    /// 役割を生産できる施設の選択肢から、展開と順位付けに使う静的な諸元を一度だけ求める。
    fn observe(
        role: ProductionRole,
        input: &RollingPlanInput,
        facilities: &[(GridPosition, Vec<usize>)],
        facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
    ) -> Self {
        let option_indices: Vec<usize> = facilities
            .iter()
            .filter_map(|(facility, _)| facility_role_options.get(&(*facility, role)).copied())
            .collect();
        // 戦闘役の兵種諸元は、施設順で最初に見つかった選択肢を代表として使う。
        let first_option = option_indices
            .first()
            .map(|&index| &input.production_options[index]);
        Self {
            cost: role.cost(),
            capture_target: role.capture_target(),
            capacity: option_indices.len(),
            min_capture_completion: min_capture_completion(role, input, &option_indices),
            is_combat_non_infantry: role.capture_target().is_none()
                && first_option
                    .is_some_and(|option| option.stats.movement_type != MovementType::Infantry),
            max_movement: first_option.map_or(0, |option| option.stats.max_movement),
            damage_by_enemy: damage_by_enemy(role, input, &option_indices),
        }
    }
}

/// 占領役は、生産可能な施設のうち最短の占領完了turnを持つ。戦闘役と不明は最大値。
fn min_capture_completion(
    role: ProductionRole,
    input: &RollingPlanInput,
    option_indices: &[usize],
) -> u32 {
    if role.capture_target().is_none() {
        return u32::MAX;
    }
    option_indices
        .iter()
        .filter_map(|&index| input.production_options[index].capture_completion_turn)
        .min()
        .unwrap_or(u32::MAX)
}

/// 敵ごとに、いずれかの施設から攻撃が届く戦闘役だけ最大基本ダメージを計上する。
fn damage_by_enemy(
    role: ProductionRole,
    input: &RollingPlanInput,
    option_indices: &[usize],
) -> Vec<u32> {
    input
        .enemies
        .iter()
        .enumerate()
        .map(|(enemy_index, enemy)| {
            let reaches = role.capture_target().is_none()
                && option_indices
                    .iter()
                    .any(|&index| option_reaches_enemy(input, index, enemy_index));
            if reaches {
                best_damage(&input.damage_chart, role.unit_type(), enemy.stats.unit_type)
            } else {
                0
            }
        })
        .collect()
}

/// 射撃投影がある選択肢は投影を正とし、投影が無い場合だけ交戦可能敵の一覧へ戻る。
fn option_reaches_enemy(input: &RollingPlanInput, option_index: usize, enemy_index: usize) -> bool {
    input
        .production_attack_projections
        .get(option_index)
        .map_or_else(
            || {
                input.production_options[option_index]
                    .engageable_enemy_indices
                    .contains(&enemy_index)
            },
            |projections| projections.get(enemy_index).is_some_and(Option::is_some),
        )
}

impl MultisetCandidate {
    fn empty(remaining_funds: u32) -> Self {
        Self {
            role_indices: Vec::new(),
            used_targets: Vec::new(),
            current_role_count: 0,
            remaining_funds,
            last_role_idx: 0,
        }
    }

    /// 役割を1つ加えた子候補を返す。資金超過、占領対象の重複、
    /// 戦闘役の施設数・重複上限超過はNoneとして展開しない。
    fn extend_with(&self, role_index: usize, meta: &RoleMetadata) -> Option<Self> {
        if meta.cost > self.remaining_funds {
            return None;
        }
        // 役割indexは非減少で追加するため、直前と同じ役割なら連続数が重複数になる。
        let role_count = if role_index == self.last_role_idx {
            self.current_role_count.saturating_add(1)
        } else {
            1
        };
        let used_targets = match meta.capture_target {
            Some(target) if self.used_targets.contains(&target) => return None,
            Some(target) => {
                let mut used_targets = self.used_targets.clone();
                used_targets.push(target);
                used_targets
            }
            None if role_count <= meta.capacity && role_count <= MAX_COMBAT_ROLE_COPIES => {
                self.used_targets.clone()
            }
            None => return None,
        };
        let mut role_indices = self.role_indices.clone();
        role_indices.push(role_index);
        Some(Self {
            role_indices,
            used_targets,
            current_role_count: role_count,
            remaining_funds: self.remaining_funds.saturating_sub(meta.cost),
            last_role_idx: role_index,
        })
    }

    fn roles(&self, roles: &[ProductionRole]) -> Vec<ProductionRole> {
        self.role_indices
            .iter()
            .map(|&index| roles[index])
            .collect()
    }
}

/// 親候補の順に、直前の役割index以降だけを加えて次のサイズ段を作る（重複組合せを1回だけ生成）。
fn expand_level(
    parents: &[MultisetCandidate],
    role_metas: &[RoleMetadata],
) -> Vec<MultisetCandidate> {
    parents
        .iter()
        .flat_map(|parent| {
            role_metas
                .iter()
                .enumerate()
                .skip(parent.last_role_idx)
                .filter_map(move |(index, meta)| parent.extend_with(index, meta))
        })
        .collect()
}

/// 候補数がビーム幅を超えた段だけ前線キー順に縮約し、縮約したかを返す。
/// キーは役割index列を含み候補間で一意なので、不安定ソートでも順序は決定的。
fn retain_multiset_beam(
    input: &RollingPlanInput,
    level: &mut Vec<MultisetCandidate>,
    role_metas: &[RoleMetadata],
) -> bool {
    if level.len() <= MULTISET_BEAM_WIDTH {
        return false;
    }
    level.sort_unstable_by_key(|candidate| {
        multiset_frontier_key(input, &candidate.role_indices, role_metas)
    });
    level.truncate(MULTISET_BEAM_WIDTH);
    true
}
