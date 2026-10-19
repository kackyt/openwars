//! campaignの非占領unitが、Squad目標を「集合して戦う参照点」として扱うことを検証する。
//! 中心1マスへの居座り・占領マスの閉塞をやめ、範囲外への無目的な後退は従来どおり拒否する。
use super::probe_tests::{assign, pos, spawn, world};
use super::*;
use crate::components::ActionCompleted;
use crate::resources::UnitType;

fn wait_target(command: &AiCommand) -> GridPosition {
    match command {
        AiCommand::Wait { target_pos } => *target_pos,
        other => panic!("Waitを期待したが {other:?}"),
    }
}

fn grid_distance(a: GridPosition, b: GridPosition) -> usize {
    a.x.abs_diff(b.x) + a.y.abs_diff(b.y)
}

/// 戦車が友軍の占領予定都市を塞いでいれば隣へ退き、同じ部隊の歩兵がその都市を占領できる。
#[test]
fn vehicle_vacates_reserved_city_and_infantry_captures_it() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(false);
    let city = pos(3, 2);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, city);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(1, 2));
    assign(&mut world, tank, MissionType::Defense, city);
    assign(&mut world, infantry, MissionType::Capture, city);

    let (entity, command) =
        decide_action(&mut world, PlayerId(1), &HashSet::from_iter([infantry])).unwrap();
    assert_eq!(entity, tank);
    let vacated = wait_target(&command);
    assert_ne!(vacated, city, "占領予定の都市に居座らない");
    assert_eq!(
        grid_distance(vacated, city),
        1,
        "参照点を支えられる隣接マスに留まる"
    );

    // 移動の適用だけを再現し、続く歩兵の判断を確認する。
    *world.get_mut::<GridPosition>(tank).unwrap() = vacated;
    world.get_mut::<ActionCompleted>(tank).unwrap().0 = true;
    let (entity, command) =
        decide_action(&mut world, PlayerId(1), &HashSet::from_iter([tank])).unwrap();
    assert_eq!(entity, infantry);
    assert!(
        matches!(command, AiCommand::Capture { target_pos } if target_pos == city),
        "空いた都市を同じ部隊の歩兵が占領する: {command:?}"
    );
}

/// 占領予定でない参照点では、中心と隣接マスの接近点が同じになり、隣への位置替えを候補に残す。
/// 参照点に働ける範囲より外へ下がる移動は、従来どおり後退として拒否する。
#[test]
fn reference_point_allows_repositioning_within_reach_but_rejects_retreat() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    crate::ai::wait_probe::stage(&mut world, "normal_tactics");
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    let evaluation = &probe["evaluation"];
    let moving = &evaluation["best_moving_wait"];
    assert!(moving.is_object(), "隣接マスへの位置替えWaitが候補に残る");
    assert_eq!(
        moving["score_parts"]["squad_approach"],
        evaluation["best_by_action"]["Wait"]["score_parts"]["squad_approach"],
        "射程内では中心と同じ接近点"
    );
    let samples = evaluation["rejection_samples"]["wait_move_away_from_campaign"]
        .as_array()
        .unwrap();
    assert!(!samples.is_empty());
    assert!(
        samples
            .iter()
            .all(|sample| sample["facts"]["candidate_grid_distance"].as_u64().unwrap() > 1),
        "拒否されるのは参照点に働けない距離への後退だけ"
    );
}

/// 占領unitは参照点ではなく施設そのものを目指すため、中心への接近点を維持する。
#[test]
fn capturer_still_targets_the_property_itself() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(8, 2));
    assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    crate::ai::wait_probe::stage(&mut world, "normal_tactics");
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert_eq!(
        wait_target(&command),
        pos(5, 2),
        "移動力いっぱい施設へ近づく"
    );
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert!(
        probe["evaluation"]["rejected_counts"]["wait_move_away_from_campaign"]
            .as_u64()
            .unwrap()
            > 0,
        "占領unitの後退拒否は従来どおり"
    );
}
