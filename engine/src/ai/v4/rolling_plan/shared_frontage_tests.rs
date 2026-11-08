//! 地上の直接戦闘部隊が、移動タイプに関係なく同じ前線マスを取り合うことの回帰試験。
use super::*;
use crate::resources::GridTopology;
use crate::resources::master_data::UnitName;

fn stats(master: &MasterDataRegistry, kind: UnitType) -> UnitStats {
    master
        .create_unit_stats(&UnitName(kind.as_str().to_owned()))
        .unwrap()
}

/// マスターデータの主・副兵装から、実対局と同じ基礎ダメージ表を作る。
fn damage_chart(master: &MasterDataRegistry) -> DamageChart {
    let mut chart = DamageChart::new();
    for (name, record) in &master.units {
        let attacker = master.unit_type_for_name(&name.0).unwrap();
        for (secondary, weapon) in [(false, &record.weapon1), (true, &record.weapon2)] {
            let Some(weapon) = weapon
                .as_ref()
                .and_then(|weapon| master.weapons.get(&UnitName(weapon.clone())))
            else {
                continue;
            };
            for (target, damage) in &weapon.damages {
                let target = master.unit_type_for_name(target).unwrap();
                if secondary {
                    chart.insert_secondary_damage(attacker, target, *damage);
                } else {
                    chart.insert_damage(attacker, target, *damage);
                }
            }
        }
    }
    chart
}

/// 敵戦車1体を囲む4マスのうち3マスに、戦車と装甲車の候補が同じ施設から出る入力。
fn input_with_shared_origins() -> RollingPlanInput {
    let mut input = super::tests::input();
    let master = MasterDataRegistry::load().unwrap();
    input.map = Arc::new(Map::new(7, 3, Terrain::Plains, GridTopology::Square));
    input.existing_units.clear();
    input.protected_units.clear();
    input.production_options.clear();
    input.enemies = vec![EnemyPlanUnit {
        entity: Some(Entity::from_raw(90)),
        stats: stats(&master, UnitType::MdTank),
        position: GridPosition { x: 4, y: 1 },
        hp: 100,
        defense_bonus: 0,
        available_turn: 0,
    }];
    for y in 0..3 {
        for kind in [UnitType::MdTank, UnitType::Recon] {
            input.production_options.push(ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: GridPosition { x: 0, y },
                    unit_type: kind,
                    build_turn: 0,
                    cost: stats(&master, kind).cost,
                },
                stats: stats(&master, kind),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
            });
        }
    }
    input.damage_chart = Arc::new(damage_chart(&master));
    input.master_data = Arc::new(master);
    input
}

#[test]
fn tank_and_armored_car_lanes_share_the_same_front_cells() {
    let input = input_with_shared_origins();
    let separate = battle_frontage_with(&input, false).unwrap();
    assert_eq!(separate.shared_direct_width, None);
    let lane_widths = separate
        .widths
        .iter()
        .filter(|(lane, _)| lane.indirect_range.is_none())
        .map(|(_, width)| *width)
        .collect::<Vec<_>>();
    // 従来の区分別幅は、同じ3つの出発マスを兵種ごとに別々に数える。
    assert_eq!(lane_widths.iter().sum::<u32>(), 6);
    let shared = battle_frontage_with(&input, true).unwrap();
    assert_eq!(shared.shared_direct_width, Some(3));
}

#[test]
fn shared_direct_width_limits_attacks_across_movement_types() {
    let mut input = input_with_shared_origins();
    let master = (*input.master_data).clone();
    input.map = Arc::new(Map::new(6, 4, Terrain::Plains, GridTopology::Square));
    input.enemies[0].position = GridPosition { x: 3, y: 1 };
    // 4体が撃っても倒れない重戦車を相手にし、撃破で攻撃数が減る影響を除く。
    input.enemies[0].stats = stats(&master, UnitType::TankZ);
    input.production_options.clear();
    input.existing_units = [
        (UnitType::MdTank, (2, 1)),
        (UnitType::Recon, (4, 1)),
        (UnitType::MdTank, (3, 0)),
        (UnitType::Recon, (3, 2)),
    ]
    .into_iter()
    .map(|(kind, (x, y))| FriendlyPlanUnit {
        stats: stats(&master, kind),
        position: GridPosition { x, y },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: vec![0],
        protection_completion_turn: None,
    })
    .collect();
    for cap in [None, Some(2)] {
        let mut catalog = SimulationCatalog::new(&input);
        catalog.frontage = Some(BattleFrontage {
            widths: HashMap::default(),
            ground_chokepoint_width: 1,
            shared_direct_width: cap,
        });
        let mut run = CombatRun::start(&catalog, &SearchState::default());
        let attacks = run.friendly_attacks(1);
        assert_eq!(attacks, cap.unwrap_or(4));
    }
}
