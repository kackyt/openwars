//! 購入候補と購入後の既存戦力で、武装輸送部隊の役割を一致させる回帰試験。
use super::*;
use crate::ai::v4::test_support::{mixed_threat_multi_factory_scan, pos};
use crate::resources::master_data::UnitName;

fn credited_existing(armed_carriers: bool, assigned: bool) -> Vec<(UnitType, u32)> {
    let mut scan = mixed_threat_multi_factory_scan();
    let recon = Entity::from_raw(201);
    let truck = Entity::from_raw(202);
    let mut chart = (*scan.damage_chart).clone();
    chart.insert_damage(UnitType::Recon, UnitType::Infantry, 70);
    scan.damage_chart = chart.into();
    for (entity, kind, hp) in [
        (recon, UnitType::Recon, 76),
        (truck, UnitType::SupplyTruck, 100),
    ] {
        let stats = scan
            .master_data
            .create_unit_stats(&UnitName(kind.as_str().to_owned()))
            .unwrap();
        scan.my_units.push(UnitSnapshot {
            entity: Some(entity),
            pos: pos(2, 2),
            stats,
            hp,
            free_cargo: 1,
        });
    }
    let player = PlayerId(1);
    let policy = ProductionPolicy::Standard;
    let mut ctx = ReachCtx::default();
    let operations = operation_builder::build_operations(&scan, &mut ctx, &[], player, policy);
    let operation = operations
        .iter()
        .find(|op| op.kind == OperationKind::Capture)
        .unwrap();
    let committed = if assigned {
        HashSet::from_iter([recon, truck])
    } else {
        HashSet::default()
    };
    let empty = HashSet::default();
    let no_staged = HashSet::default();
    let input = CombatInputs {
        policy,
        scan: &scan,
        op: operation,
        used_facilities: &empty,
        committed_combat_entities: &committed,
        staged_combat_entities: &no_staged,
        remaining_funds: scan.funds,
        require_self_deployment: false,
        player_id: player,
    };
    let mut draft = collect_combat_targets(&input, &mut ctx).expect("敵が観測されるfixture");
    collect_existing_force_with_roles(&input, &mut ctx, &mut draft, armed_carriers);
    draft
        .existing_units
        .iter()
        .map(|unit| (unit.stats.unit_type, unit.hp))
        .collect()
}

#[test]
fn assigned_armed_carrier_is_credited_after_purchase_with_current_hp() {
    assert_eq!(credited_existing(true, true), vec![(UnitType::Recon, 76)]);
}

#[test]
fn unassigned_armed_carrier_is_not_counted_for_another_operation() {
    assert!(credited_existing(true, false).is_empty());
}

#[test]
fn disabled_role_prototype_keeps_the_original_existing_force_filter() {
    assert!(credited_existing(false, true).is_empty());
}
