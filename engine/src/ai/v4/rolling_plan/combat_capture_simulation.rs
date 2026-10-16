//! RollingPlanのcombat_capture_simulation責務。
use super::force_package::select_target;
use super::model::{PlannedCapturePurchase, PlannedCombatPurchase, SimCaptureContract};
use super::plan_ranking::{
    direct_enemy_remaining_hp_after_assignment, direct_fire_lane_count,
    direct_fire_saturates_target, select_front_breakthrough_target, uses_small_map_tactical_rules,
};
use super::{
    CampaignTurnForecast, Entity, ForcePackagePlan, GridPosition, HashSet, PlannedPurchase,
    RollingPlanInput, SearchState, SimEnemy, SimFriendly, SimulationCatalog, TargetForecast,
    calculate_damage_formula, sim_production_friendly,
};

/// 初期戦力を複製し、各turnの自軍行動→敵応答→集計を同じ順で進める。
/// 初期戦力を複製し、各turnの自軍行動→敵応答→集計を同じ順で進める。
pub(super) fn simulate_state_with_catalog(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    state: &SearchState,
    search_turns: u32,
) -> ForcePackagePlan {
    let mut simulation = CombatCaptureSimulation::new(input, catalog, state, search_turns);
    for turn in 1..=search_turns {
        simulation.advance_turn(input, turn);
        if simulation.is_complete(input, turn) {
            break;
        }
    }
    simulation.finish(input, catalog, state, search_turns)
}

struct SimulationActuals {
    first_attack_turn: Option<u32>,
    front_breakthrough_turn: Option<u32>,
    deadline_target_first_attack_turn: Option<u32>,
    target_first_attack_turns: Vec<Option<u32>>,
    turn_forecasts: Vec<CampaignTurnForecast>,
    combat_purchases: Vec<PlannedCombatPurchase>,
    assigned_combat_purchases: HashSet<PlannedPurchase>,
    blocked_next_turn_production_facilities: HashSet<GridPosition>,
    protected_survivors_at_completion: Option<usize>,
    direct_damage_by_target: Vec<u32>,
    existing_indirect_damage_by_target: Vec<u32>,
    produced_indirect_damage_by_target: Vec<u32>,
}
struct CombatCaptureSimulation {
    purchases: Vec<PlannedPurchase>,
    friendlies: Vec<SimFriendly>,
    protected_units: Vec<SimFriendly>,
    capture_friendly_indices: Vec<usize>,
    enemies: Vec<SimEnemy>,
    direct_enemy_remaining_hp: Vec<u32>,
    direct_front_shortfall: usize,
    actuals: SimulationActuals,
}
struct TurnSnapshot {
    enemy_arrival_hp: u32,
    enemy_hp_before: u32,
    friendly_hp_before: u32,
}
#[derive(Clone, Copy)]
struct FriendlyAttack {
    turn: u32,
    target_index: usize,
    attack_profile: super::FriendlyAttackProfile,
    damage: u32,
}

impl CombatCaptureSimulation {
    /// 初期戦力と候補の占領契約を一体ずつ同じ順で用意する。
    fn new(
        input: &RollingPlanInput,
        catalog: &SimulationCatalog,
        state: &SearchState,
        search_turns: u32,
    ) -> Self {
        let purchases: Vec<_> = state
            .option_indices
            .iter()
            .map(|index| input.production_options[*index].purchase)
            .collect();
        let mut friendlies = catalog.existing_units.clone();
        let protected_units = catalog.protected_units.clone();
        let mut capture_friendly_indices = Vec::new();
        for index in &state.option_indices {
            let friendly_index = friendlies.len();
            friendlies.push(catalog.production_units[*index].clone());
            if input.production_options[*index].capture_target.is_some() {
                capture_friendly_indices.push(friendly_index);
            }
        }
        let enemies = catalog.enemies.clone();
        let direct_enemy_remaining_hp = if uses_small_map_tactical_rules(input) {
            direct_enemy_remaining_hp_after_assignment(&friendlies, &enemies, search_turns)
        } else {
            vec![0; enemies.len()]
        };
        let direct_front_shortfall = direct_enemy_remaining_hp
            .iter()
            .filter(|remaining| **remaining > 0)
            .count();
        let first_attack_turn = None;
        let front_breakthrough_turn = None;
        let deadline_target_first_attack_turn = None;
        let target_first_attack_turns = vec![None; enemies.len()];
        let turn_forecasts = Vec::new();
        let combat_purchases = Vec::new();
        let assigned_combat_purchases = HashSet::default();
        let blocked_next_turn_production_facilities = HashSet::default();
        let protected_survivors_at_completion = None;
        // 砲を増やすかは、同じ標的への砲数ではなく、直接部隊・既存砲・今回の砲が
        // 実際にどの敵HPを削ったかで比較する。候補ごとに既存simulationの攻撃処理で
        // 一度だけ加算するので、別の盤面simulationは増やさない。
        let direct_damage_by_target = vec![0_u32; enemies.len()];
        let existing_indirect_damage_by_target = vec![0_u32; enemies.len()];
        let produced_indirect_damage_by_target = vec![0_u32; enemies.len()];
        Self {
            purchases,
            friendlies,
            protected_units,
            capture_friendly_indices,
            enemies,
            direct_enemy_remaining_hp,
            direct_front_shortfall,
            actuals: SimulationActuals {
                first_attack_turn,
                front_breakthrough_turn,
                deadline_target_first_attack_turn,
                target_first_attack_turns,
                turn_forecasts,
                combat_purchases,
                assigned_combat_purchases,
                blocked_next_turn_production_facilities,
                protected_survivors_at_completion,
                direct_damage_by_target,
                existing_indirect_damage_by_target,
                produced_indirect_damage_by_target,
            },
        }
    }

    /// 敵増援の到着と行動前HPを記録する。
    fn turn_snapshot(&self, turn: u32) -> TurnSnapshot {
        let enemy_arrival_hp = self
            .enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn == turn)
            .map(|enemy| enemy.source.hp)
            .sum();
        let enemy_hp_before: u32 = self
            .enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_before: u32 = self
            .friendlies
            .iter()
            .chain(self.protected_units.iter())
            .map(|friendly| friendly.hp)
            .sum();
        TurnSnapshot {
            enemy_arrival_hp,
            enemy_hp_before,
            friendly_hp_before,
        }
    }

    /// 自軍の占領・射撃、敵の最良応答、占領生存と予実をこの順で反映する。
    fn advance_turn(&mut self, input: &RollingPlanInput, turn: u32) {
        let before = self.turn_snapshot(turn);
        let lethal_attacker_counts = lethal_attacker_counts(&self.friendlies, &self.enemies, turn);
        let front_breakthrough_target = (input.exact_property_control
            && input.interdiction_deadlines.is_empty()
            // 一撃で排除できる相性担当がいる場合、その担当を容易な別標的への
            // 集中射撃へ強制しない。単独撃破が無い局面だけ火力集中を組む。
            && lethal_attacker_counts.iter().all(|count| *count == 0))
        .then(|| select_front_breakthrough_target(&self.friendlies, &self.enemies, turn))
        .flatten();
        if front_breakthrough_target.is_some() {
            self.actuals.front_breakthrough_turn.get_or_insert(turn);
        }
        let attack_count = self.execute_friendly_actions(
            input,
            turn,
            front_breakthrough_target,
            &lethal_attacker_counts,
        );
        self.execute_enemy_responses(turn);
        self.observe_capture_survival(input, turn);
        self.record_turn(turn, before, attack_count);
    }

    /// 一体のHPと行動回数を占領と戦闘で共有し、攻撃数を集計する。
    fn execute_friendly_actions(
        &mut self,
        input: &RollingPlanInput,
        turn: u32,
        front_breakthrough_target: Option<usize>,
        lethal_attacker_counts: &[usize],
    ) -> u32 {
        let mut attack_count = 0_u32;
        for friendly in &mut self.friendlies {
            if self.actuals.execute_friendly_action(
                input,
                friendly,
                &mut self.enemies,
                turn,
                front_breakthrough_target,
                lethal_attacker_counts,
            ) {
                attack_count = attack_count.saturating_add(1);
            }
        }
        attack_count
    }

    /// 敵は走査順のまま、その時点で最も有利な友軍へ応答する。
    fn execute_enemy_responses(&mut self, turn: u32) {
        for (enemy_index, enemy) in self.enemies.iter_mut().enumerate() {
            resolve_enemy_attack(
                enemy_index,
                enemy,
                &mut self.friendlies,
                &mut self.protected_units,
                turn,
            );
        }
    }

    /// 敵応答後のHPで既存占領兵と保護対象の生存を観測する。
    fn observe_capture_survival(&mut self, input: &RollingPlanInput, turn: u32) {
        for friendly in &mut self.friendlies {
            if friendly.protection_completion_turn == Some(turn) {
                friendly.survived_protection_completion = Some(friendly.hp > 0);
            }
        }
        if input.capture_completion_turn == Some(turn) {
            self.actuals.protected_survivors_at_completion = Some(
                self.protected_units
                    .iter()
                    .filter(|unit| unit.hp > 0)
                    .count(),
            );
        }
    }

    /// 行動前後の差分を一turnの予測履歴へ記録する。
    fn record_turn(&mut self, turn: u32, before: TurnSnapshot, attack_count: u32) {
        let TurnSnapshot {
            enemy_arrival_hp,
            enemy_hp_before,
            friendly_hp_before,
        } = before;
        let enemy_hp_after: u32 = self
            .enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_after: u32 = self
            .friendlies
            .iter()
            .chain(self.protected_units.iter())
            .map(|friendly| friendly.hp)
            .sum();
        self.actuals.turn_forecasts.push(CampaignTurnForecast {
            turn,
            enemy_arrival_hp,
            enemy_hp_removed: enemy_hp_before.saturating_sub(enemy_hp_after),
            friendly_hp_lost: friendly_hp_before.saturating_sub(friendly_hp_after),
            attack_count,
        });
    }

    /// 敵全滅だけで止めず、選択した全占領契約と既存占領時刻を待つ。
    fn is_complete(&self, input: &RollingPlanInput, turn: u32) -> bool {
        let all_selected_captures_complete = self.capture_friendly_indices.iter().all(|index| {
            self.friendlies[*index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| contract.completed_turn.is_some())
        });
        let existing_capture_complete = input
            .capture_completion_turn
            .is_none_or(|capture_turn| turn >= capture_turn);
        self.enemies.iter().all(|enemy| enemy.hp == 0)
            && all_selected_captures_complete
            && existing_capture_complete
    }

    /// 占領契約と戦闘結果を集計して購入候補の評価値を組み立てる。
    fn finish(
        self,
        input: &RollingPlanInput,
        catalog: &SimulationCatalog,
        state: &SearchState,
        search_turns: u32,
    ) -> ForcePackagePlan {
        let Self {
            purchases,
            friendlies,
            protected_units,
            capture_friendly_indices,
            enemies,
            direct_enemy_remaining_hp,
            direct_front_shortfall,
            actuals,
        } = self;
        let SimulationActuals {
            first_attack_turn,
            front_breakthrough_turn,
            deadline_target_first_attack_turn,
            target_first_attack_turns,
            turn_forecasts,
            combat_purchases,
            assigned_combat_purchases: _,
            blocked_next_turn_production_facilities,
            protected_survivors_at_completion,
            direct_damage_by_target,
            existing_indirect_damage_by_target,
            produced_indirect_damage_by_target,
        } = actuals;
        let elimination_turn = if enemies.is_empty() {
            Some(0)
        } else {
            enemies
                .iter()
                .map(|enemy| enemy.destroyed_turn)
                .collect::<Option<Vec<_>>>()
                .and_then(|turns| turns.into_iter().max())
        };
        let CaptureOutcome {
            capture_purchases,
            selected_capture_completion_turn,
            protected_survivor_count,
            deadline_capture_survivor_count,
            protected_force_survives,
        } = capture_outcome(
            input,
            &friendlies,
            &protected_units,
            &capture_friendly_indices,
            protected_survivors_at_completion,
            search_turns,
        );
        // 固定2ターンを足さず、実campaignのPickup/Transit/Drop/Capture ETAと残敵排除の
        // 遅い方を採る。輸送編成がまだ存在しない場合は占領完了を予測しない。
        let occupation_turn = elimination_turn
            .zip(selected_capture_completion_turn)
            .filter(|_| protected_force_survives)
            .map(|(elimination, capture)| elimination.max(capture));
        let CombatOutcome {
            expected_loss,
            required_overmatch_value,
            surviving_combat_value,
            direct_fire_lane_count,
        } = combat_outcome(input, &friendlies, &protected_units, &combat_purchases);
        ForcePackagePlan {
            purchases,
            capture_purchases,
            combat_purchases,
            target_forecasts: enemies
                .into_iter()
                .map(|enemy| TargetForecast {
                    entity: enemy.source.entity,
                    unit_type: enemy.source.stats.unit_type,
                    available_turn: enemy.source.available_turn,
                    initial_hp: enemy.source.hp,
                    remaining_hp: enemy.hp,
                    destroyed_turn: enemy.destroyed_turn,
                })
                .collect(),
            turn_forecasts,
            feasible: elimination_turn.is_some() && protected_force_survives,
            first_attack_turn,
            front_breakthrough_turn,
            deadline_target_first_attack_turn,
            target_first_attack_turns,
            elimination_turn,
            occupation_turn,
            production_cost: state.cost,
            expected_loss,
            blocked_next_turn_production_slots: blocked_next_turn_production_facilities.len(),
            direct_fire_lane_count,
            direct_front_shortfall,
            direct_enemy_remaining_hp,
            direct_damage_by_target,
            existing_indirect_damage_by_target,
            produced_indirect_damage_by_target,
            surviving_combat_value,
            required_overmatch_value,
            overmatch_ready: surviving_combat_value >= required_overmatch_value,
            protected_unit_count: input.protected_units.len()
                + catalog
                    .existing_units
                    .iter()
                    .filter(|unit| unit.protection_completion_turn.is_some())
                    .count()
                + capture_friendly_indices.len(),
            protected_survivor_count,
            deadline_capture_survivor_count,
            required_capture_survivor_count: input.required_capture_survivors,
            candidates_considered: 0,
            candidates_pruned: 0,
            search_truncated: false,
        }
    }
}

impl SimulationActuals {
    /// 占領を試し、消費可能な行動があれば選択した敵へ一度だけ攻撃する。
    #[allow(clippy::too_many_arguments)]
    fn execute_friendly_action(
        &mut self,
        input: &RollingPlanInput,
        friendly: &mut SimFriendly,
        enemies: &mut [SimEnemy],
        turn: u32,
        front_breakthrough_target: Option<usize>,
        lethal_attacker_counts: &[usize],
    ) -> bool {
        if friendly.hp == 0 || turn < friendly.available_turn {
            return false;
        }
        if capture_if_ready(friendly, enemies, turn) || friendly.attacks_left == 0 {
            return false;
        }
        let Some((target_index, attack_profile)) = self.choose_friendly_target(
            input,
            friendly,
            enemies,
            turn,
            front_breakthrough_target,
            lethal_attacker_counts,
        ) else {
            return false;
        };
        let target = &mut enemies[target_index];
        let damage = calculate_damage_formula(
            attack_profile.base_damage,
            friendly.hp,
            target.source.defense_bonus,
            false,
        );
        if damage == 0 {
            return false;
        }
        self.record_attack(
            input,
            friendly,
            target,
            FriendlyAttack {
                turn,
                target_index,
                attack_profile,
                damage,
            },
        );
        target.hp = target.hp.saturating_sub(damage);
        friendly.attacks_left = friendly.attacks_left.saturating_sub(1);
        if target.hp == 0 {
            target.destroyed_turn = Some(turn);
            return true;
        }
        // 与damageと初撃ETAは従来の編成比較軸を維持し、反撃可否だけを
        // 計画交戦距離における本番武器選択へ委譲する。
        let counter_base = attack_profile.incoming_damage;
        if counter_base > 0 {
            let counter =
                calculate_damage_formula(counter_base, target.hp, 0, true).saturating_add(10);
            friendly.hp = friendly.hp.saturating_sub(counter);
        }
        true
    }

    /// 占領阻害敵→突破口→既存の標的評価の順を維持する。
    #[allow(clippy::too_many_arguments)]
    fn choose_friendly_target(
        &self,
        input: &RollingPlanInput,
        friendly: &SimFriendly,
        enemies: &[SimEnemy],
        turn: u32,
        front_breakthrough_target: Option<usize>,
        lethal_attacker_counts: &[usize],
    ) -> Option<(usize, super::FriendlyAttackProfile)> {
        let capture_blocker = friendly
            .capture_contract
            .as_ref()
            .filter(|contract| contract.completed_turn.is_none())
            .and_then(|contract| contract.blocking_enemy_index);

        let contract_target = capture_blocker.and_then(|target_index| {
            let enemy = enemies.get(target_index)?;
            let profile = friendly
                .attack_profiles
                .get(target_index)
                .copied()
                .flatten()?;
            (enemy.hp > 0 && turn >= enemy.source.available_turn && turn >= profile.ready_turn)
                .then_some((target_index, profile))
        });
        let concentrated_target = contract_target.or_else(|| {
            front_breakthrough_target.and_then(|target_index| {
                let enemy = enemies.get(target_index)?;
                let profile = friendly
                    .attack_profiles
                    .get(target_index)
                    .copied()
                    .flatten()?;
                (enemy.hp > 0 && turn >= enemy.source.available_turn && turn >= profile.ready_turn)
                    .then_some((target_index, profile))
            })
        });
        concentrated_target.or_else(|| {
            select_target(
                friendly,
                enemies,
                turn,
                &input.interdiction_deadlines,
                &self.target_first_attack_turns,
                lethal_attacker_counts,
            )
        })
    }

    /// 初撃、購入役割、施設占有と火力寄与を同じ攻撃から一度だけ観測する。
    fn record_attack(
        &mut self,
        input: &RollingPlanInput,
        friendly: &SimFriendly,
        target: &SimEnemy,
        attack: FriendlyAttack,
    ) {
        let FriendlyAttack {
            turn,
            target_index,
            attack_profile,
            damage,
        } = attack;
        self.first_attack_turn.get_or_insert(turn);
        if input.deadline_target_index == Some(target_index) {
            self.deadline_target_first_attack_turn.get_or_insert(turn);
        }
        self.target_first_attack_turns[target_index].get_or_insert(turn);
        if let (Some(purchase), Some(target_entity)) = (friendly.purchase, target.source.entity)
            && self.assigned_combat_purchases.insert(purchase)
        {
            self.combat_purchases.push(PlannedCombatPurchase {
                purchase,
                target: target_entity,
            });
        }
        if turn == 1
            && let Some(purchase) = friendly.purchase
            && purchase.build_turn == 0
            && attack_profile.firing_position == Some(purchase.facility)
            && !attack_profile.requires_movement
        {
            // 生産直後のunitが次手番も施設上から攻撃するなら、その手番の生産枠は
            // 物理的に使えない。価格換算せず、失うslot数として保持する。
            self.blocked_next_turn_production_facilities
                .insert(purchase.facility);
        }
        if friendly.stats.min_range <= 1 {
            self.direct_damage_by_target[target_index] =
                self.direct_damage_by_target[target_index].saturating_add(damage);
        } else if friendly.purchase.is_some() {
            self.produced_indirect_damage_by_target[target_index] =
                self.produced_indirect_damage_by_target[target_index].saturating_add(damage);
        } else {
            self.existing_indirect_damage_by_target[target_index] =
                self.existing_indirect_damage_by_target[target_index].saturating_add(damage);
        }
    }
}

/// 生存HPから実占領力を求め、占領したturnは攻撃しない。
fn capture_if_ready(friendly: &mut SimFriendly, enemies: &[SimEnemy], turn: u32) -> bool {
    let capture_blocker = friendly
        .capture_contract
        .as_ref()
        .filter(|contract| contract.completed_turn.is_none())
        .and_then(|contract| contract.blocking_enemy_index);
    let blocker_cleared = capture_blocker
        .and_then(|index| enemies.get(index))
        .is_none_or(|enemy| enemy.hp == 0);
    let capture_ready = friendly.capture_contract.as_ref().is_some_and(|contract| {
        contract.completed_turn.is_none() && turn >= contract.arrival_turn && blocker_cleared
    });
    if capture_ready {
        let power = super::super::property_control::capture_power_from_hp(friendly.hp);
        let contract = friendly
            .capture_contract
            .as_mut()
            .expect("占領可能判定済みの契約が存在する");
        // 占領と攻撃は同一手番に併用しない。被弾後HPから実占領力を求め、
        // 固定2回で完了したことにしない。
        contract.remaining_durability = contract.remaining_durability.saturating_sub(power);
        if contract.remaining_durability == 0 {
            contract.completed_turn = Some(turn);
            contract.survived_at_completion = true;
        }
        return true;
    }
    false
}

/// 敵一体の最良応答を現在HPへ適用する。
fn resolve_enemy_attack(
    enemy_index: usize,
    enemy: &mut SimEnemy,
    friendlies: &mut [SimFriendly],
    protected_units: &mut [SimFriendly],
    turn: u32,
) {
    if enemy.hp == 0 || enemy.attacks_left == 0 || turn < enemy.source.available_turn {
        return;
    }
    let combat_target = best_enemy_target(friendlies, enemy_index, enemy, turn);
    let protected_target = best_enemy_target(protected_units, enemy_index, enemy, turn);
    let target = match (combat_target, protected_target) {
        (Some((_index, damage)), Some((protected_index, protected_damage)))
            if protected_damage > damage =>
        {
            (true, protected_index, protected_damage)
        }
        (Some((index, damage)), _) => (false, index, damage),
        (None, Some((index, damage))) => (true, index, damage),
        (None, None) => {
            return;
        }
    };
    if target.0 {
        protected_units[target.1].hp = protected_units[target.1].hp.saturating_sub(target.2);
    } else {
        friendlies[target.1].hp = friendlies[target.1].hp.saturating_sub(target.2);
    }
    if target.2 == 0 {
        return;
    }
    enemy.attacks_left = enemy.attacks_left.saturating_sub(1);
}

/// 接敵時刻を満たす候補からdamageと残HPで最も有利な標的を選ぶ。
fn best_enemy_target(
    friendlies: &[SimFriendly],
    enemy_index: usize,
    enemy: &SimEnemy,
    turn: u32,
) -> Option<(usize, u32)> {
    friendlies
        .iter()
        .enumerate()
        .filter(|(_, target)| target.hp > 0 && turn >= target.available_turn)
        .filter_map(|(index, target)| {
            let profile = target.enemy_attack_profiles[enemy_index]?;
            if turn < profile.contact_turn {
                return None;
            }
            let damage =
                calculate_damage_formula(profile.base_damage, enemy.hp, 0, true).saturating_add(10);
            Some((index, damage))
        })
        .max_by_key(|(index, damage)| (*damage, 100_u32.saturating_sub(friendlies[*index].hp)))
}

struct CaptureOutcome {
    capture_purchases: Vec<PlannedCapturePurchase>,
    selected_capture_completion_turn: Option<u32>,
    protected_survivor_count: usize,
    deadline_capture_survivor_count: usize,
    protected_force_survives: bool,
}
/// 同じ実unitで達成した占領完了時刻と期限内の生存数を集計する。
fn capture_outcome(
    input: &RollingPlanInput,
    friendlies: &[SimFriendly],
    protected_units: &[SimFriendly],
    capture_friendly_indices: &[usize],
    protected_survivors_at_completion: Option<usize>,
    search_turns: u32,
) -> CaptureOutcome {
    let capture_purchases = capture_friendly_indices
        .iter()
        .filter_map(|index| {
            let friendly = &friendlies[*index];
            let contract = friendly.capture_contract.as_ref()?;
            Some(PlannedCapturePurchase {
                purchase: friendly.purchase?,
                target: contract.target,
                completion_turn: contract.completed_turn,
                blocking_enemy_index: contract.blocking_enemy_index,
                survived_at_completion: contract.survived_at_completion,
            })
        })
        .collect::<Vec<_>>();
    let selected_capture_completion_turn = capture_purchases
        .iter()
        .filter_map(|purchase| purchase.completion_turn)
        .chain(input.capture_completion_turn)
        .max();
    let selected_capture_survivor_count = capture_friendly_indices
        .iter()
        .filter(|index| {
            friendlies[**index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| contract.survived_at_completion)
        })
        .count();
    let existing_protected_survivor_count = friendlies
        .iter()
        .filter(|friendly| friendly.protection_completion_turn.is_some())
        .filter(|friendly| {
            friendly
                .survived_protection_completion
                .unwrap_or(friendly.hp > 0)
        })
        .count();
    // 物件別の合同探索では、対象物件を実際に取り切った同じunitだけを契約成立へ数える。
    // 戦闘用cloneと保護用cloneへ分けず、購入費・HP・行動を一体分に保つ。
    let protected_survivor_count = protected_survivors_at_completion
        .unwrap_or_else(|| protected_units.iter().filter(|unit| unit.hp > 0).count())
        .saturating_add(existing_protected_survivor_count)
        .saturating_add(selected_capture_survivor_count)
        + if input.exact_property_control {
            0
        } else {
            friendlies
                .iter()
                .filter(|unit| {
                    unit.capture_contract.is_none()
                        && unit.protection_completion_turn.is_none()
                        && unit.stats.can_capture
                        && unit.hp > 0
                })
                .count()
        };
    let capture_deadline = input.hard_deadline.unwrap_or(search_turns);
    let deadline_capture_survivor_count = capture_friendly_indices
        .iter()
        .filter(|index| {
            friendlies[**index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| {
                    contract.survived_at_completion
                        && contract
                            .completed_turn
                            .is_some_and(|turn| turn <= capture_deadline)
                })
        })
        .count();
    // 占領完了ETAがまだ無いことは、占領役の生存要求が無いことを意味しない。
    // 物件別Interdictでは撃破完了前にETAを確定できないため、要求数そのものを判定する。
    let protected_force_survives = input.required_capture_survivors == 0
        || protected_survivor_count >= input.required_capture_survivors;
    CaptureOutcome {
        capture_purchases,
        selected_capture_completion_turn,
        protected_survivor_count,
        deadline_capture_survivor_count,
        protected_force_survives,
    }
}

struct CombatOutcome {
    expected_loss: u32,
    required_overmatch_value: u32,
    surviving_combat_value: u32,
    direct_fire_lane_count: usize,
}
/// 実損耗、将来増援後の保持戦力と直接射点を集計する。
fn combat_outcome(
    input: &RollingPlanInput,
    friendlies: &[SimFriendly],
    protected_units: &[SimFriendly],
    combat_purchases: &[PlannedCombatPurchase],
) -> CombatOutcome {
    let expected_loss =
        friendlies
            .iter()
            .chain(protected_units.iter())
            .fold(0_u32, |total, unit| {
                let lost_hp = unit.initial_hp.saturating_sub(unit.hp);
                total.saturating_add(unit.stats.cost.saturating_mul(lost_hp) / 100)
            });
    // `entity=None` はExpected scenarioとして投入した将来増援である。撃破後に
    // その半分の戦闘価値を残せる案を優先し、相手を倒した直後に前線が空になる
    // 最小編成を避ける。ただし達成不能でもfeasibleを偽にしない。
    let required_overmatch_value = input
        .enemies
        .iter()
        .filter(|enemy| enemy.entity.is_none())
        .map(|enemy| enemy.stats.cost.saturating_mul(enemy.hp) / 100)
        .fold(0_u32, u32::saturating_add)
        / 2;
    let surviving_combat_value = friendlies
        .iter()
        // 搭載能力は非戦闘の証拠ではない。実際に観測敵へ攻撃profileを持つ
        // 生存unitだけを戦闘価値へ数え、武装装甲車を輸送役として除外しない。
        .filter(|unit| unit.hp > 0 && unit.attack_profiles.iter().any(Option::is_some))
        .map(|unit| unit.stats.cost.saturating_mul(unit.hp) / 100)
        .fold(0_u32, u32::saturating_add);
    let direct_fire_lane_count = if uses_small_map_tactical_rules(input) {
        direct_fire_lane_count(input, combat_purchases)
    } else {
        0
    };
    CombatOutcome {
        expected_loss,
        required_overmatch_value,
        surviving_combat_value,
        direct_fire_lane_count,
    }
}

/// 現在手番に各敵を単独撃破できる友軍数。
///
/// 「戦車なら装甲車を一撃で落とせるが、他兵科では落とせない」のような相性担当を
/// 容易な歩兵へ浪費しないための希少度であり、兵種名や固定評価点は使わない。
pub(super) fn lethal_attacker_counts(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    turn: u32,
) -> Vec<usize> {
    enemies
        .iter()
        .enumerate()
        .map(|(enemy_index, enemy)| {
            if enemy.hp == 0 || turn < enemy.source.available_turn {
                return 0;
            }
            friendlies
                .iter()
                .filter(|friendly| {
                    friendly.hp > 0 && friendly.attacks_left > 0 && turn >= friendly.available_turn
                })
                .filter(|friendly| {
                    friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()
                        .is_some_and(|profile| {
                            turn >= profile.ready_turn
                                && calculate_damage_formula(
                                    profile.base_damage,
                                    friendly.hp,
                                    enemy.source.defense_bonus,
                                    false,
                                ) >= enemy.hp
                        })
                })
                .count()
        })
        .collect()
}

impl ForcePackagePlan {
    pub(crate) fn capture_target_for(&self, purchase: PlannedPurchase) -> Option<GridPosition> {
        self.capture_purchases
            .iter()
            .find(|assignment| assignment.purchase == purchase)
            .map(|assignment| assignment.target)
    }

    pub(crate) fn combat_target_for(&self, purchase: PlannedPurchase) -> Option<Entity> {
        self.combat_purchases
            .iter()
            .find(|assignment| assignment.purchase == purchase)
            .map(|assignment| assignment.target)
    }

    /// 生き残った敵HPを、マスターデータ上の再調達価値で比例換算する。
    /// 同じ100HPでも歩兵と戦車を同価値にせず、固定の兵種点ではなく盤面に存在する
    /// unit自身の価格と残HPだけを使う。
    pub(super) fn remaining_enemy_value(&self, input: &RollingPlanInput) -> u64 {
        self.target_forecasts
            .iter()
            .zip(&input.enemies)
            .map(|(forecast, enemy)| {
                u64::from(enemy.stats.cost).saturating_mul(u64::from(forecast.remaining_hp)) / 100
            })
            .sum()
    }

    pub(super) fn has_indirect_purchase(&self, input: &RollingPlanInput) -> bool {
        self.purchases.iter().any(|purchase| {
            input
                .production_options
                .iter()
                .find(|option| option.purchase == *purchase)
                .is_some_and(|option| option.stats.min_range > 1)
        })
    }

    /// 間接部隊が、同じ局地目標を担当する直接部隊の外側から射点を増やせるか。
    ///
    /// 戦力額の総和ではなく、各候補の実際の射点・到達turn・担当敵だけを見る。
    /// したがって、歩兵・装甲車・戦車のどれがscreenを担うかは相性シミュレーションに
    /// 任せ、砲撃役だけが直接部隊の任務を置き換える案を避けられる。
    pub(super) fn indirect_fire_has_local_support(&self, input: &RollingPlanInput) -> bool {
        self.purchases.iter().all(|purchase| {
            let Some(option_index) = input
                .production_options
                .iter()
                .position(|option| option.purchase == *purchase)
            else {
                return true;
            };
            let option = &input.production_options[option_index];
            if option.stats.min_range <= 1 {
                return true;
            }
            input
                .production_attack_projections
                .get(option_index)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(enemy_index, projection)| {
                    (*projection).map(|edge| (enemy_index, edge))
                })
                // 工場上から撃ち続ける砲は、次手番の生産枠を失うため前線の火力枠にはしない。
                .filter(|(_, edge)| edge.firing_position != option.purchase.facility)
                .any(|(enemy_index, edge)| {
                    direct_fire_saturates_target(input, self, enemy_index, edge.ready_turn)
                })
        })
    }

    pub(super) fn interdiction_first_attack_turn(&self) -> Option<u32> {
        self.deadline_target_first_attack_turn
            .or(self.first_attack_turn)
    }

    pub(super) fn interdiction_remaining_hp(&self, target_index: Option<usize>) -> u32 {
        target_index
            .and_then(|index| self.target_forecasts.get(index))
            .map_or_else(|| self.remaining_hp(), |target| target.remaining_hp)
    }

    pub(super) fn missed_interdiction_contracts(&self, deadlines: &[(usize, u32)]) -> usize {
        deadlines
            .iter()
            .filter(|(index, deadline)| {
                !self
                    .target_first_attack_turns
                    .get(*index)
                    .and_then(|turn| *turn)
                    .is_some_and(|turn| turn <= *deadline)
            })
            .count()
    }

    /// 期限が早い契約から見た累積未達数。
    ///
    /// 敵index順の初撃turnをそのまま辞書順比較すると、盤面走査で先に現れた無関係な敵へ
    /// 一発入れる案が有利になる。同じ期限の契約は同価値として束ね、明示された期限だけから
    /// 「この時点までに何件止め損ねたか」を作る。
    pub(super) fn interdiction_deadline_shortfall_profile(
        &self,
        deadlines: &[(usize, u32)],
    ) -> Vec<usize> {
        let mut checkpoints = deadlines
            .iter()
            .map(|(_, deadline)| *deadline)
            .collect::<Vec<_>>();
        checkpoints.sort_unstable();
        checkpoints.dedup();
        checkpoints
            .into_iter()
            .map(|checkpoint| {
                deadlines
                    .iter()
                    .filter(|(_, deadline)| *deadline <= checkpoint)
                    .filter(|(index, deadline)| {
                        !self
                            .target_first_attack_turns
                            .get(*index)
                            .and_then(|turn| *turn)
                            .is_some_and(|turn| turn <= *deadline)
                    })
                    .count()
            })
            .collect()
    }

    /// 明示された妨害対象だけの初撃時刻。非契約敵への早い小ダメージは比較へ混ぜない。
    pub(super) fn interdiction_attack_turn_profile(&self, deadlines: &[(usize, u32)]) -> Vec<u32> {
        let mut turns = deadlines
            .iter()
            .map(|(index, _)| {
                self.target_first_attack_turns
                    .get(*index)
                    .and_then(|turn| *turn)
                    .unwrap_or(u32::MAX)
            })
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
    }

    pub(super) fn interdiction_contracts_satisfied(&self, deadlines: &[(usize, u32)]) -> bool {
        !deadlines.is_empty() && self.missed_interdiction_contracts(deadlines) == 0
    }

    pub(super) fn capture_survivor_shortfall(&self, required: usize) -> usize {
        required.saturating_sub(self.protected_survivor_count)
    }

    pub(super) fn property_contracts_satisfied(
        &self,
        deadlines: &[(usize, u32)],
        required_capture_survivors: usize,
    ) -> bool {
        self.completed_property_contracts(deadlines, required_capture_survivors)
            >= required_capture_survivors
    }

    pub(super) fn completed_property_contracts(
        &self,
        deadlines: &[(usize, u32)],
        required_capture_survivors: usize,
    ) -> usize {
        let combat_completed = if deadlines.is_empty() {
            self.target_forecasts
                .iter()
                .filter(|target| target.destroyed_turn.is_some())
                .count()
        } else {
            deadlines
                .len()
                .saturating_sub(self.missed_interdiction_contracts(deadlines))
        };
        self.protected_survivor_count
            .min(required_capture_survivors)
            .min(combat_completed)
    }

    pub(super) fn front_breakthrough_rank(&self) -> (bool, u32) {
        (
            self.front_breakthrough_turn.is_none(),
            self.front_breakthrough_turn.unwrap_or(u32::MAX),
        )
    }

    pub(super) fn capture_completion_profile(&self) -> Vec<u32> {
        let mut turns = self
            .capture_purchases
            .iter()
            .map(|assignment| assignment.completion_turn.unwrap_or(u32::MAX))
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
    }

    /// 物件ごとに対応付けた妨害敵を排除し、同じ占領役が生存して完了したレーン数。
    ///
    /// 撃破時刻と占領時刻を並べ替えて対応させると、左翼の容易な敵を倒しただけで
    /// 中央レーンまで開通したことになる。生産入力で確定した物件―敵のidentityを使う。
    pub(super) fn cleared_capture_lane_count(&self) -> usize {
        self.capture_purchases
            .iter()
            .filter(|assignment| {
                let Some(completion_turn) = assignment.completion_turn else {
                    return false;
                };
                if !assignment.survived_at_completion {
                    return false;
                }
                assignment.blocking_enemy_index.is_none_or(|enemy_index| {
                    self.target_forecasts
                        .get(enemy_index)
                        .and_then(|target| target.destroyed_turn)
                        .is_some_and(|destroyed_turn| destroyed_turn <= completion_turn)
                })
            })
            .count()
            .min(self.required_capture_survivor_count)
    }

    /// 一体目だけでなく、観測した前線敵を何手番で順次掃討できるかを比較する。
    pub(super) fn target_destruction_profile(&self) -> Vec<u32> {
        let mut turns = self
            .target_forecasts
            .iter()
            .map(|target| target.destroyed_turn.unwrap_or(u32::MAX))
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
    }

    /// 対応する敵の排除と占領完了が両方そろう時刻を、レーンごとに比較する。
    pub(super) fn property_contract_completion_profile(&self) -> Vec<u32> {
        let mut turns = self
            .capture_purchases
            .iter()
            .map(|assignment| {
                if !assignment.survived_at_completion {
                    return u32::MAX;
                }
                let capture = assignment.completion_turn.unwrap_or(u32::MAX);
                let destruction = assignment.blocking_enemy_index.map_or(0, |enemy_index| {
                    self.target_forecasts
                        .get(enemy_index)
                        .and_then(|target| target.destroyed_turn)
                        .unwrap_or(u32::MAX)
                });
                capture.max(destruction)
            })
            .collect::<Vec<_>>();
        // `[] < [turn]` によって契約を一件も引き受けない案が有利にならないよう、
        // 不足契約は未達時刻で埋める。同じ入力ではrequired数が共通なので比較可能。
        if turns.len() < self.required_capture_survivor_count {
            turns.resize(self.required_capture_survivor_count, u32::MAX);
        }
        turns.sort_unstable();
        turns
    }
}

impl SimulationCatalog {
    pub(super) fn new_extended(input: &RollingPlanInput) -> Self {
        // 共通初期化を使い、生産unitにだけ契約と実経路の射撃予測を追加する。
        Self::new_with_production_initializer(input, |option_index, option| {
            let mut simulated =
                sim_production_friendly(input, option, option.capture_target.is_none());
            simulated.purchase = Some(option.purchase);
            debug_assert!(option.capture_completion_turn.is_none_or(|completion| {
                option
                    .capture_arrival_turn
                    .is_some_and(|arrival| completion >= arrival)
            }));
            simulated.capture_contract = option.capture_target.and_then(|target| {
                Some(SimCaptureContract {
                    target,
                    arrival_turn: option.capture_arrival_turn?,
                    remaining_durability: option.capture_durability?,
                    blocking_enemy_index: option.capture_blocking_enemy_index,
                    completed_turn: None,
                    survived_at_completion: false,
                })
            });
            // 生産判断側で計算した実経路ETAがあれば、格子距離による近似を
            // 上書きする。山・川・橋・最小射程を無視した快速判定を防ぐ。
            if let Some(exact_projections) = input.production_attack_projections.get(option_index) {
                let mut profiles = simulated.attack_profiles.to_vec();
                for (enemy_index, profile) in profiles.iter_mut().enumerate() {
                    match (
                        profile.as_mut(),
                        exact_projections.get(enemy_index).copied().flatten(),
                    ) {
                        (Some(profile), Some(projection)) => {
                            profile.ready_turn = projection.ready_turn;
                            profile.firing_position = Some(projection.firing_position);
                            profile.requires_movement = projection.requires_movement;
                        }
                        (Some(_), None) => *profile = None,
                        (None, _) => {}
                    }
                }
                simulated.attack_profiles = profiles.into();
            }
            simulated
        })
    }
}
