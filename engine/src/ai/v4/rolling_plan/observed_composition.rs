//! 観測敵だけに対する交換計算と敵別の未対処率。兵種名・配合比・追加の評価係数は使わない。
use super::*;
use crate::systems::combat::get_detailed_expected_damage;
use std::cmp::Ordering;

pub(in crate::ai::v4) fn enabled() -> bool {
    !crate::ai::code_path::is_extended() && crate::ai::features::observed_composition_enabled()
}

#[derive(Clone, Copy)]
pub(super) struct Combatant<'a> {
    pub stats: &'a UnitStats,
    pub hp: u32,
    pub ammo: (u32, u32),
    pub defense: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Exchange {
    pub attacker_hp: u32,
    pub defender_hp: u32,
    pub attacker_ammo: (u32, u32),
    pub defender_ammo: (u32, u32),
    pub damage: u32,
    pub counter_damage: u32,
}

fn consume(mut ammo: (u32, u32), slot: u32) -> (u32, u32) {
    if slot == 1 {
        ammo.0 = ammo.0.saturating_sub(1);
    } else {
        ammo.1 = ammo.1.saturating_sub(1);
    }
    ammo
}

/// 乱数加算を除く本番の武器選択・反撃手順。双方の攻撃前HPから計算する。
pub(super) fn exchange(
    master: &MasterDataRegistry,
    chart: &DamageChart,
    attacker: Combatant<'_>,
    defender: Combatant<'_>,
    distance: u32,
) -> Option<Exchange> {
    if attacker.hp == 0 || defender.hp == 0 {
        return None;
    }
    let (damage, (slot, _, indirect)) = get_detailed_expected_damage(
        attacker.stats,
        attacker.hp,
        attacker.ammo,
        defender.stats,
        defender.defense,
        distance,
        master,
        chart,
        false,
    )?;
    let counter = (!indirect)
        .then(|| {
            get_detailed_expected_damage(
                defender.stats,
                defender.hp,
                defender.ammo,
                attacker.stats,
                attacker.defense,
                distance,
                master,
                chart,
                true,
            )
        })
        .flatten();
    let counter_damage = counter.as_ref().map_or(0, |(damage, _)| *damage);
    Some(Exchange {
        attacker_hp: attacker.hp.saturating_sub(counter_damage),
        defender_hp: defender.hp.saturating_sub(damage),
        attacker_ammo: consume(attacker.ammo, slot),
        defender_ammo: counter.map_or(defender.ammo, |(_, (slot, _, _))| {
            consume(defender.ammo, slot)
        }),
        damage: damage.min(defender.hp),
        counter_damage: counter_damage.min(attacker.hp),
    })
}

#[derive(Clone, Copy, Debug)]
struct Fraction {
    remaining: u32,
    initial: u32,
}
impl PartialEq for Fraction {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Fraction {}
impl Ord for Fraction {
    fn cmp(&self, other: &Self) -> Ordering {
        (u64::from(self.remaining) * u64::from(other.initial))
            .cmp(&(u64::from(other.remaining) * u64::from(self.initial)))
    }
}
impl PartialOrd for Fraction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn profile(input: &RollingPlanInput, plan: &ForcePackagePlan, contested: bool) -> Vec<Fraction> {
    let mut values = plan
        .target_forecasts
        .iter()
        .enumerate()
        .filter(|(index, target)| {
            target.entity.is_some()
                && target.initial_hp > 0
                && (!contested || input.hold_contestants.get(*index).copied().unwrap_or(true))
        })
        .map(|(_, target)| Fraction {
            remaining: target.remaining_hp,
            initial: target.initial_hp,
        })
        .collect::<Vec<_>>();
    values.sort_unstable_by(|a, b| b.cmp(a));
    values
}

/// 小さい方が良い。別の敵への大きなHP削りで、無傷の観測敵を帳消しにしない。
pub(in crate::ai::v4) fn compare(
    input: &RollingPlanInput,
    a: &ForcePackagePlan,
    b: &ForcePackagePlan,
) -> Ordering {
    let shortfall = |p: &ForcePackagePlan| {
        if input.capture_completion_turn.is_some() {
            input
                .required_capture_survivors
                .max(1)
                .saturating_sub(p.protected_survivor_count)
        } else {
            0
        }
    };
    shortfall(a)
        .cmp(&shortfall(b))
        .then_with(|| (!a.feasible).cmp(&!b.feasible))
        .then_with(|| profile(input, a, true).cmp(&profile(input, b, true)))
        .then_with(|| profile(input, a, false).cmp(&profile(input, b, false)))
        .then_with(|| a.expected_loss.cmp(&b.expected_loss))
        .then_with(|| {
            a.first_attack_turn
                .unwrap_or(u32::MAX)
                .cmp(&b.first_attack_turn.unwrap_or(u32::MAX))
        })
        .then_with(|| a.production_cost.cmp(&b.production_cost))
}

/// 現在の射撃機会を敵へ割り当てる。合法な交換の実損益と撃破を使い、架空の将来火力を足さない。
pub(super) fn select_target(
    input: &RollingPlanInput,
    friendly: &SimFriendly,
    state: &UnitCombatState,
    enemies: &[SimEnemy],
    states: &[EnemyCombatState],
    direct_attacks: &[u32],
    turn: u32,
) -> Option<(usize, FriendlyAttackProfile)> {
    let mut best = None;
    for (index, profile) in friendly.attack_profiles.iter().enumerate() {
        let Some(profile) = *profile else {
            continue;
        };
        if states[index].hp == 0
            || turn < enemies[index].source.available_turn
            || turn < profile.ready_turn
            || profile
                .firing_slots
                .is_some_and(|slots| direct_attacks[index] >= slots)
        {
            continue;
        }
        let enemy = &enemies[index].source;
        let Some(result) = exchange(
            &input.master_data,
            &input.damage_chart,
            Combatant {
                stats: &friendly.stats,
                hp: state.hp,
                ammo: state.ammo,
                defense: 0,
            },
            Combatant {
                stats: &enemy.stats,
                hp: states[index].hp,
                ammo: states[index].ammo,
                defense: enemy.defense_bonus,
            },
            profile.attack_distance,
        ) else {
            continue;
        };
        let net = i64::from(enemy.stats.cost) * i64::from(result.damage)
            - i64::from(friendly.stats.cost) * i64::from(result.counter_damage);
        let key = (
            result.defender_hp == 0,
            net,
            result.damage,
            std::cmp::Reverse(index),
        );
        if best.as_ref().is_none_or(|(current, _, _)| key > *current) {
            best = Some((key, index, profile));
        }
    }
    best.map(|(_, index, profile)| (index, profile))
}

/// 敵側も現在弾薬と合法な武器を確認してから標的を選ぶ。撃てない候補で打ち切らない。
pub(super) fn enemy_target(
    input: &RollingPlanInput,
    run: &CombatRun<'_>,
    enemy_index: usize,
    turn: u32,
    shielded_min_max_range: Option<u32>,
) -> Option<(bool, usize, Exchange)> {
    let enemy = &run.catalog.enemies[enemy_index].source;
    let enemy_state = &run.enemy_states[enemy_index];
    let combat = (0..run.friendly_states.len())
        .map(|i| (false, i, run.friendly(i), &run.friendly_states[i]));
    let protected = run
        .catalog
        .protected_units
        .iter()
        .zip(&run.protected_states)
        .enumerate()
        .map(|(i, (unit, state))| (true, i, unit, state));
    let mut best = None;
    for (protected, index, unit, state) in combat.chain(protected) {
        if state.hp == 0
            || turn < unit.available_turn
            || (!protected
                && shielded_min_max_range
                    .is_some_and(|range| unit.stats.min_range > 1 && unit.stats.max_range >= range))
        {
            continue;
        }
        let Some(profile) = unit.enemy_attack_profiles[enemy_index] else {
            continue;
        };
        if turn < profile.contact_turn {
            continue;
        }
        let Some(result) = exchange(
            &input.master_data,
            &input.damage_chart,
            Combatant {
                stats: &enemy.stats,
                hp: enemy_state.hp,
                ammo: enemy_state.ammo,
                defense: enemy.defense_bonus,
            },
            Combatant {
                stats: &unit.stats,
                hp: state.hp,
                ammo: state.ammo,
                defense: 0,
            },
            profile.attack_distance,
        ) else {
            continue;
        };
        let net = i64::from(unit.stats.cost) * i64::from(result.damage)
            - i64::from(enemy.stats.cost) * i64::from(result.counter_damage);
        let key = (
            result.defender_hp == 0,
            net,
            result.damage,
            std::cmp::Reverse(state.hp),
        );
        if best
            .as_ref()
            .is_none_or(|(current, _, _, _)| key > *current)
        {
            best = Some((key, protected, index, result));
        }
    }
    best.map(|(_, protected, index, result)| (protected, index, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;
    fn master_and_chart() -> (MasterDataRegistry, DamageChart) {
        let master = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        for (name, record) in &master.units {
            let kind = master.unit_type_for_name(&name.0).unwrap();
            for (secondary, weapon) in [(false, &record.weapon1), (true, &record.weapon2)] {
                if let Some(weapon) = weapon
                    .as_ref()
                    .and_then(|name| master.weapons.get(&UnitName(name.clone())))
                {
                    for (target, damage) in &weapon.damages {
                        let target = master.unit_type_for_name(target).unwrap();
                        if secondary {
                            chart.insert_secondary_damage(kind, target, *damage);
                        } else {
                            chart.insert_damage(kind, target, *damage);
                        }
                    }
                }
            }
        }
        (master, chart)
    }
    fn stats(master: &MasterDataRegistry, kind: UnitType) -> UnitStats {
        master
            .create_unit_stats(&UnitName(kind.as_str().to_owned()))
            .unwrap()
    }
    fn full(stats: &UnitStats) -> Combatant<'_> {
        Combatant {
            stats,
            hp: 100,
            ammo: (stats.max_ammo1, stats.max_ammo2),
            defense: 0,
        }
    }
    #[test]
    fn recon_is_effective_against_infantry_but_fragile_against_observed_tank() {
        let (master, chart) = master_and_chart();
        let recon = stats(&master, UnitType::Recon);
        for (target, damage, counter, hp) in [
            (UnitType::Infantry, 79, 16, 84),
            (UnitType::MdTank, 6, 88, 12),
        ] {
            let defender = stats(&master, target);
            let result = exchange(&master, &chart, full(&recon), full(&defender), 1).unwrap();
            assert_eq!(
                (result.damage, result.counter_damage, result.attacker_hp),
                (damage, counter, hp)
            );
        }
    }
    #[test]
    fn wounded_attacker_loses_damage_and_empty_magazine_cannot_fire() {
        let (master, chart) = master_and_chart();
        let recon = stats(&master, UnitType::Recon);
        let tank = stats(&master, UnitType::MdTank);
        let result = exchange(
            &master,
            &chart,
            Combatant {
                hp: 12,
                ..full(&recon)
            },
            full(&tank),
            1,
        )
        .unwrap();
        assert_eq!(result.damage, 1);
        assert_eq!(result.attacker_hp, 0);
        assert!(
            exchange(
                &master,
                &chart,
                Combatant {
                    ammo: (0, 0),
                    ..full(&recon)
                },
                full(&tank),
                1
            )
            .is_none()
        );
    }
    #[test]
    fn primary_weapon_and_counter_use_the_same_rules_as_actual_combat() {
        let (master, chart) = master_and_chart();
        let tank = stats(&master, UnitType::MdTank);
        let infantry = stats(&master, UnitType::Infantry);
        let result = exchange(&master, &chart, full(&tank), full(&infantry), 1).unwrap();
        assert_eq!((result.damage, result.counter_damage), (54, 6));
        assert_eq!(result.attacker_ammo.0, tank.max_ammo1 - 1);
        let fallback = exchange(
            &master,
            &chart,
            Combatant {
                ammo: (0, tank.max_ammo2),
                ..full(&tank)
            },
            full(&infantry),
            1,
        )
        .unwrap();
        assert_eq!(fallback.damage, 77);
    }
    fn fight_input(target: UnitType, hp: u32) -> RollingPlanInput {
        let (master, chart) = master_and_chart();
        let recon = stats(&master, UnitType::Recon);
        let enemy = stats(&master, target);
        let mut input = super::super::tests::input();
        input.master_data = Arc::new(master);
        input.damage_chart = Arc::new(chart);
        input.production_options.clear();
        input.protected_units.clear();
        input.existing_units = vec![FriendlyPlanUnit {
            stats: recon,
            position: GridPosition { x: 1, y: 0 },
            hp,
            available_turn: 0,
            engageable_enemy_indices: vec![0],
            protection_completion_turn: None,
        }];
        input.enemies = vec![EnemyPlanUnit {
            entity: Some(Entity::from_raw(50)),
            stats: enemy,
            position: GridPosition { x: 2, y: 0 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 0,
        }];
        input
    }
    #[test]
    fn rollout_uses_pre_attack_counter_hp_and_counter_ammunition() {
        let input = fight_input(UnitType::Infantry, 100);
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        assert_eq!(run.friendly_attacks(&input, 1), 1);
        assert_eq!(
            (run.friendly_states[0].hp, run.enemy_states[0].hp),
            (84, 21)
        );
        assert_eq!(run.enemy_states[0].ammo.0, 8);
        assert_eq!(run.friendly_attacks(&input, 2), 1);
        assert_eq!((run.friendly_states[0].hp, run.enemy_states[0].hp), (80, 0));
    }
    #[test]
    fn enemy_attack_receives_the_same_counter_response() {
        let input = fight_input(UnitType::Infantry, 100);
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        run.friendly_attacks(&input, 1);
        run.enemy_attacks(&input, 1);
        assert_eq!((run.friendly_states[0].hp, run.enemy_states[0].hp), (80, 0));
    }
    #[test]
    fn a_single_round_of_ammunition_cannot_be_spent_twice() {
        let mut input = fight_input(UnitType::Infantry, 100);
        input.existing_units[0].stats.max_ammo1 = 1;
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        assert_eq!(run.friendly_attacks(&input, 1), 1);
        assert_eq!(run.friendly_attacks(&input, 2), 0);
        assert_eq!(run.enemy_states[0].hp, 21);
    }

    #[test]
    fn current_ammunition_is_loaded_without_refilling_existing_units() {
        let mut input = fight_input(UnitType::MdTank, 100);
        let mut scan = crate::ai::v4::test_support::mixed_threat_multi_factory_scan();
        let entity = Entity::from_raw(77);
        let source = &input.existing_units[0];
        scan.my_units = vec![crate::ai::v4::UnitSnapshot {
            entity: Some(entity),
            pos: source.position,
            stats: source.stats.clone(),
            hp: source.hp,
            free_cargo: 1,
        }];
        let ammunition = HashMap::from_iter([(entity, (0, 0)), (Entity::from_raw(50), (0, 3))]);
        crate::ai::v4::combat_input::apply_observed_ammunition(&mut input, &scan, &ammunition);
        assert_eq!(
            scan.my_units[0].stats.max_ammo1, 9,
            "盤面の能力値は変更しない"
        );
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        assert_eq!(run.friendly_states[0].ammo, (0, 0));
        assert_eq!(run.enemy_states[0].ammo, (0, 3));
        assert_eq!(run.friendly_attacks(&input, 1), 0);
    }
    #[test]
    fn enemy_skips_an_illegal_primary_target_and_uses_a_legal_secondary_target() {
        let mut input = fight_input(UnitType::Mech, 100);
        input.map = Arc::new(Map::new(
            6,
            4,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        input.existing_units.push(FriendlyPlanUnit {
            stats: stats(&input.master_data, UnitType::Bcopters),
            position: GridPosition { x: 1, y: 1 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: vec![0],
            protection_completion_turn: None,
        });
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        run.enemy_states[0].ammo = (0, 3);
        run.enemy_attacks(&input, 1);
        assert_eq!(run.friendly_states[0].hp, 100);
        assert_eq!(run.friendly_states[1].hp, 67);
        assert_eq!(run.enemy_states[0].ammo.1, 2);
    }
    #[test]
    fn a_remote_observed_enemy_does_not_prevent_local_hold_completion() {
        let mut input = fight_input(UnitType::Infantry, 100);
        input.map = Arc::new(Map::new(
            30,
            4,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        input.hold_window = Some(2);
        input.hold_contestants = vec![true, false];
        input.enemies.push(EnemyPlanUnit {
            entity: Some(Entity::from_raw(51)),
            stats: stats(&input.master_data, UnitType::MdTank),
            position: GridPosition { x: 29, y: 3 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 0,
        });
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        run.advance_to(&input, 2);
        let plan = run.into_plan(&input, &SearchState::default());
        assert!(plan.feasible);
        assert_eq!(plan.target_forecasts[1].remaining_hp, 100);
    }
    #[test]
    fn ambiguous_source_with_missing_ammo_cannot_borrow_another_units_magazine() {
        let mut input = fight_input(UnitType::Infantry, 100);
        let mut scan = crate::ai::v4::test_support::mixed_threat_multi_factory_scan();
        let source = &input.existing_units[0];
        let first = Entity::from_raw(77);
        let second = Entity::from_raw(78);
        scan.my_units = [first, second]
            .into_iter()
            .map(|entity| crate::ai::v4::UnitSnapshot {
                entity: Some(entity),
                pos: source.position,
                stats: source.stats.clone(),
                hp: source.hp,
                free_cargo: 1,
            })
            .collect();
        crate::ai::v4::combat_input::apply_observed_ammunition(
            &mut input,
            &scan,
            &HashMap::from_iter([(first, (9, 0))]),
        );
        assert_eq!(input.existing_units[0].stats.max_ammo1, 0);
    }
    #[test]
    fn meeting_the_hold_deadline_beats_a_cheaper_late_result() {
        let input = fight_input(UnitType::Infantry, 100);
        let mut late = super::super::simulate_state(&input, &SearchState::default(), 1);
        late.feasible = false;
        late.production_cost = 1;
        let mut on_time = late.clone();
        on_time.feasible = true;
        on_time.production_cost = 10_000;
        assert!(compare(&input, &on_time, &late).is_lt());
    }
    fn input_with_purchases(count: usize) -> (RollingPlanInput, Vec<PlannedPurchase>) {
        let mut input = fight_input(UnitType::Infantry, 100);
        let recon = input.existing_units.remove(0).stats;
        input.production_options.clear();
        let mut purchases = Vec::new();
        for y in 0..count {
            let purchase = PlannedPurchase {
                facility: GridPosition { x: 1, y },
                unit_type: UnitType::Recon,
                build_turn: 0,
                cost: recon.cost,
            };
            input.production_options.push(ProductionPlanOption {
                purchase,
                stats: recon.clone(),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
            });
            purchases.push(purchase);
        }
        input.map = Arc::new(Map::new(
            6,
            count.max(1),
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        input.enemies[0].position = GridPosition { x: 2, y: 0 };
        (input, purchases)
    }

    #[test]
    fn prediction_ledger_records_value_exchange_of_a_single_shot() {
        let (input, _) = input_with_purchases(1);
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        // 購入が無い入力でも、既存戦力の記録先が友軍と同じ長さで作れること。
        run.ledger = Some(vec![UnitLedger::default(); run.friendly_states.len()]);
        assert!(run.ledger.as_ref().unwrap().is_empty());
        let state = SearchState {
            option_indices: vec![0],
            ..SearchState::default()
        };
        let mut run = CombatRun::start(&catalog, &state);
        run.ledger = Some(vec![UnitLedger::default(); run.friendly_states.len()]);
        assert_eq!(run.friendly_attacks(&input, 1), 1);
        let ledger = run.ledger.as_ref().unwrap()[0];
        // 装甲車は軽歩兵へ79、反撃は16。価値は購入額×HP/100。
        assert_eq!((ledger.shots, ledger.first_shot_turn), (1, Some(1)));
        assert_eq!(ledger.dealt_value, 1_000 * 79 / 100);
        assert_eq!(ledger.taken_value, 4_200 * 16 / 100);
        assert_eq!(ledger.death_turn, None);
    }

    #[test]
    fn prediction_ledger_totals_match_the_simulated_exchange() {
        let (input, purchases) = input_with_purchases(2);
        let predicted = predict_with_observed_rules(&input, &purchases);
        assert_eq!(predicted.len(), 2);
        assert_eq!(predicted[0].purchase, purchases[0]);
        let dealt: u32 = predicted.iter().map(|unit| unit.ledger.dealt_value).sum();
        // 1体目の射撃(79)と敵手番の反撃で敵(HP100・購入額1,000)が全滅する。
        // 各HPは1つの与価値にだけ帰属するので、合計は敵の価値と一致する。
        assert_eq!(dealt, 1_000, "predicted={predicted:?}");
        let shots: u32 = predicted.iter().map(|unit| unit.ledger.shots).sum();
        assert_eq!(shots, 1, "敵が倒れた後は撃たない。predicted={predicted:?}");
        assert_eq!(predicted[1].ledger.dealt_value, 0);
        assert!(predicted.iter().all(|unit| unit.simulated_turns >= 1));
    }

    #[test]
    fn unmatched_purchase_is_not_given_a_prediction() {
        let (input, mut purchases) = input_with_purchases(1);
        purchases.push(PlannedPurchase {
            facility: GridPosition { x: 5, y: 0 },
            unit_type: UnitType::Recon,
            build_turn: 0,
            cost: 4_200,
        });
        assert_eq!(predict_with_observed_rules(&input, &purchases).len(), 1);
    }

    #[test]
    fn dead_attacker_cannot_contribute_a_later_shot() {
        let input = fight_input(UnitType::MdTank, 12);
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        assert_eq!(run.friendly_attacks(&input, 1), 1);
        assert_eq!(run.friendly_states[0].hp, 0);
        assert_eq!(run.friendly_attacks(&input, 2), 0);
        assert_eq!(run.enemy_states[0].hp, 99);
    }
    #[test]
    fn an_untouched_observed_enemy_cannot_be_hidden_by_large_damage_elsewhere() {
        let input = fight_input(UnitType::MdTank, 100);
        let mut a = super::super::simulate_state(&input, &SearchState::default(), 1);
        let mut target = a.target_forecasts[0].clone();
        target.initial_hp = 100;
        target.remaining_hp = 0;
        a.target_forecasts[0].remaining_hp = 100;
        a.target_forecasts.push(target);
        let mut b = a.clone();
        for target in &mut b.target_forecasts {
            target.remaining_hp = 80;
        }
        assert_eq!(compare(&input, &b, &a), Ordering::Less);
    }
    fn fixed_force_against(
        targets: &[UnitType],
        force: &[UnitType],
    ) -> (RollingPlanInput, ForcePackagePlan) {
        let (master, chart) = master_and_chart();
        let mut input = super::super::tests::input();
        input.map = Arc::new(Map::new(
            6,
            4,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        input.production_options.clear();
        input.protected_units.clear();
        input.enemies = targets
            .iter()
            .enumerate()
            .map(|(i, kind)| EnemyPlanUnit {
                entity: Some(Entity::from_raw(100 + i as u32)),
                stats: stats(&master, *kind),
                position: GridPosition { x: 2, y: i },
                hp: 100,
                defense_bonus: 0,
                available_turn: 0,
            })
            .collect();
        let positions = [(1, 0), (1, 1), (0, 0), (0, 1), (0, 2)];
        input.existing_units = force
            .iter()
            .enumerate()
            .map(|(i, kind)| FriendlyPlanUnit {
                stats: stats(&master, *kind),
                position: GridPosition {
                    x: positions[i].0,
                    y: positions[i].1,
                },
                hp: 100,
                available_turn: 0,
                engageable_enemy_indices: (0..targets.len()).collect(),
                protection_completion_turn: None,
            })
            .collect();
        input.master_data = Arc::new(master);
        input.damage_chart = Arc::new(chart);
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = None;
        catalog.observed_composition = true;
        let mut result =
            super::super::simulate_state_with_catalog(&input, &catalog, &SearchState::default(), 6);
        result.production_cost = input.existing_units.iter().map(|u| u.stats.cost).sum();
        (input, result)
    }
    #[test]
    fn an_observed_tank_changes_the_result_of_a_recon_only_force() {
        let (input, recon) = fixed_force_against(
            &[UnitType::Infantry, UnitType::MdTank],
            &[UnitType::Recon; 5],
        );
        let (_, mixed) = fixed_force_against(
            &[UnitType::Infantry, UnitType::MdTank],
            &[UnitType::Recon, UnitType::MdTank],
        );
        assert!(
            compare(&input, &mixed, &recon).is_lt(),
            "mixed={mixed:?} recon={recon:?}"
        );
    }
    #[test]
    fn infantry_only_does_not_require_a_tank_for_diversity() {
        let (input, recon) = fixed_force_against(&[UnitType::Infantry], &[UnitType::Recon; 2]);
        let (_, tank) = fixed_force_against(&[UnitType::Infantry], &[UnitType::MdTank; 2]);
        assert!(compare(&input, &recon, &tank).is_lt());
    }
    #[test]
    fn indirect_shot_has_no_counter_at_legal_range() {
        let (master, chart) = master_and_chart();
        let gun = stats(&master, UnitType::HeavySpGun);
        let tank = stats(&master, UnitType::MdTank);
        let result = exchange(&master, &chart, full(&gun), full(&tank), 3).unwrap();
        assert_eq!((result.damage, result.counter_damage), (84, 0));
        assert!(exchange(&master, &chart, full(&gun), full(&tank), 1).is_none());
    }
}
