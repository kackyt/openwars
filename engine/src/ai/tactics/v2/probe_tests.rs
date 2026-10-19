//! 判断規則を変えず、実際に通った棄却・選択理由を観測できることを検証する。
use super::*;
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::operation_assignment::{
    OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
};
use crate::ai::planning::assignment::{LedgerKey, OperationLedger};
use crate::ai::squad::MissionPhase;
use crate::ai::wait_probe::{WaitProbe, WaitProbeSettings};
use crate::components::{ActionCompleted, Ammo, Faction, Fuel, HasMoved, Health, Property};
use crate::resources::master_data::{MasterDataRegistry, UnitName};
use crate::resources::{DamageChart, GridTopology, Map, MatchState, Terrain, UnitType};
use std::collections::BTreeSet;

pub(super) fn pos(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

pub(super) fn world(enabled: bool) -> World {
    let (mut world, _schedule) = crate::setup::create_world();
    let mut map = Map::new(25, 5, Terrain::Plains, GridTopology::Square);
    map.set_terrain(3, 2, Terrain::City).unwrap();
    world.insert_resource(IslandMap::analyze(&map));
    world.insert_resource(map);
    world.insert_resource(MatchState::default());
    world.insert_resource(MasterDataRegistry::load().unwrap());
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Tank, UnitType::Infantry, 70);
    chart.insert_secondary_damage(UnitType::Tank, UnitType::Infantry, 70);
    chart.insert_damage(UnitType::Infantry, UnitType::Tank, 5);
    chart.insert_secondary_damage(UnitType::Infantry, UnitType::Tank, 5);
    world.insert_resource(chart);
    world.spawn((
        pos(3, 2),
        Property::new(Terrain::City, Some(PlayerId(2)), 200),
    ));
    let mut ai = crate::ai::PlayerAiSettings::new();
    ai.set_version(PlayerId(1), crate::ai::AiVersion::V4);
    world.insert_resource(ai);
    world.insert_resource(SquadManager::default());
    world.insert_resource(UnitOperationRegistry::default());
    world.resource_mut::<WaitProbe>().settings = WaitProbeSettings {
        enabled,
        ..Default::default()
    };
    world
}

pub(super) fn spawn(
    world: &mut World,
    player: PlayerId,
    kind: UnitType,
    at: GridPosition,
) -> Entity {
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&UnitName(kind.as_str().to_string()))
        .unwrap();
    let ammo = Ammo {
        ammo1: stats.max_ammo1,
        max_ammo1: stats.max_ammo1,
        ammo2: stats.max_ammo2,
        max_ammo2: stats.max_ammo2,
    };
    let fuel = Fuel {
        current: stats.max_fuel,
        max: stats.max_fuel,
    };
    // 比較用World間で同じIDを与え、保存対象からユニットが欠落することを防ぐ。
    let id = crate::components::UnitId(uuid::Uuid::from_u128(world.entities().len() as u128 + 100));
    world
        .spawn((
            id,
            Faction(player),
            stats,
            ammo,
            fuel,
            at,
            Health {
                current: 100,
                max: 100,
            },
            HasMoved(false),
            ActionCompleted(false),
        ))
        .id()
}

pub(super) fn assign(
    world: &mut World,
    entity: Entity,
    mission: MissionType,
    target: GridPosition,
) {
    let mut manager = world.remove_resource::<SquadManager>().unwrap();
    let id = {
        let mut proposals = OperationLedger::proposals(&mut manager);
        let mut squad = proposals.create_owned(
            mission,
            PlayerId(1),
            BTreeSet::from_iter([entity]),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(target);
        squad.plan_mut().target_island = Some(IslandId(0));
        squad.plan_mut().phase = MissionPhase::MovingToTarget;
        squad.id
    };
    world.insert_resource(manager);
    world.resource_mut::<UnitOperationRegistry>().apply_assign(
        LedgerKey::fixture(),
        entity,
        UnitOperationAssignment {
            owner: OperationOwner::Campaign {
                player_id: PlayerId(1),
                island_id: IslandId(0),
            },
            squad_id: Some(id),
            role: OperationUnitRole::Member,
            assigned_turn: 1,
        },
    );
}

#[test]
fn stationary_tank_wait_reports_actual_retreat_filter_and_center_score() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    crate::ai::wait_probe::stage(&mut world, "normal_tactics");
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert_eq!(entity, tank);
    assert!(matches!(command,AiCommand::Wait{target_pos} if target_pos==pos(3,2)));
    assert!(
        crate::ai::wait_probe::take_issued(&mut world).is_none(),
        "選択だけでは発行扱いにしない"
    );
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert_eq!(probe["selection_stage"], "standard_tactics");
    assert_eq!(probe["intended_move"], false);
    assert!(
        probe["evaluation"]["rejected_counts"]["wait_move_away_from_campaign"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        probe["evaluation"]["best_by_action"]["Wait"]["score_parts"]["squad_approach"],
        300_000
    );
    assert_eq!(
        probe["evaluation"]["context"]["campaign_goal"]["occupants"][0]["unit_id"],
        tank.to_bits()
    );
}

#[test]
fn infantry_wait_reports_the_friendly_blocker_not_a_missing_job() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(2, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    let (entity, command) =
        decide_action(&mut world, PlayerId(1), &HashSet::from_iter([tank])).unwrap();
    assert_eq!(entity, infantry);
    assert!(matches!(command,AiCommand::Wait{target_pos} if target_pos==pos(2,2)));
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert_eq!(probe["evaluation"]["context"]["mission"], "Capture");
    assert_eq!(
        probe["evaluation"]["context"]["campaign_goal"]["occupants"][0]["unit_id"],
        tank.to_bits()
    );
    assert_eq!(
        probe["evaluation"]["context"]["campaign_goal"]["capture_claimants"][0]["unit_id"],
        infantry.to_bits()
    );
}

#[test]
fn filtered_unit_still_contributes_to_global_non_wait_alternative() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    let other = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(18, 2));
    spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(19, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    assign(&mut world, other, MissionType::Defense, pos(0, 0));
    world
        .resource_mut::<WaitProbe>()
        .settings
        .units
        .insert(tank.to_bits());
    crate::ai::wait_probe::begin_step(&mut world);
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert_eq!(entity, tank);
    assert!(matches!(command, AiCommand::Wait { .. }));
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert_eq!(
        probe["global_best_non_wait"]["alternative"]["unit_id"],
        other.to_bits()
    );
    assert_eq!(
        probe["global_best_non_wait"]["alternative"]["action"],
        "Attack"
    );
    assert_eq!(probe["global_best_non_wait"]["reason"], "higher_score");
    assert_eq!(probe["coverage"]["detail_units"], 1);
    assert_eq!(probe["coverage"]["evaluated_units"], 2);
}

#[test]
fn probe_on_off_keeps_actor_command_and_actual_decision_identical() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut results = Vec::new();
    for enabled in [false, true] {
        let mut world = world(enabled);
        let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
        let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(2, 2));
        assign(&mut world, tank, MissionType::Defense, pos(3, 2));
        assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
        crate::ai::wait_probe::begin_step(&mut world);
        let (entity, command) =
            decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        results.push((entity.to_bits(), format!("{command:?}")));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn no_ammo_is_distinguished_from_a_legal_attack_losing_by_score() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(4, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    {
        let mut ammo = world.get_mut::<Ammo>(tank).unwrap();
        ammo.ammo1 = 0;
        ammo.ammo2 = 0;
    }
    crate::ai::wait_probe::begin_step(&mut world);
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert!(matches!(command, AiCommand::Wait { .. }));
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert!(probe["global_best_non_wait"].is_null());
    assert!(
        probe["evaluation"]["rejected_counts"]["attack_weapon_no_ammo"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        probe["evaluation"]["rejected_counts"]["attack_target_no_usable_weapon"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn departure_restriction_does_not_claim_that_uncreated_moves_were_tested() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(2, 2));
    assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
    {
        let mut manager = world.resource_mut::<SquadManager>();
        for mut squad in manager.edit_each() {
            squad.plan_mut().departure_authorized = false;
        }
    }
    crate::ai::wait_probe::begin_step(&mut world);
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert_eq!(probe["evaluation"]["generated_candidate_tiles"], 1);
    assert_eq!(
        probe["evaluation"]["rejected_counts"]["candidate_generation_departure_restricted"],
        1
    );
    assert!(probe["evaluation"]["rejected_counts"]["wait_move_away_from_campaign"].is_null());
}

#[test]
fn ranged_after_move_is_observed_at_the_real_weapon_gate() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let artillery = spawn(&mut world, PlayerId(1), UnitType::Artillery, pos(3, 2));
    spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(10, 2));
    assign(&mut world, artillery, MissionType::Defense, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert!(
        probe["evaluation"]["rejected_counts"]["attack_ranged_after_move"]
            .as_u64()
            .unwrap()
            > 0
    );
}

/// 実際の発行イベントとschedule後の保存対象状態を、OFF/ON/絞込ONで比較する。
#[test]
fn probe_modes_preserve_every_issued_actor_and_game_state_until_turn_end() {
    use crate::events::{
        AttackUnitCommand, CapturePropertyCommand, MoveUnitCommand, WaitUnitCommand,
    };
    use base64::prelude::*;
    use bevy_ecs::event::EventCursor;
    let mut runs = Vec::new();
    for mode in 0..3 {
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, mut schedule) =
            crate::setup::initialize_world_from_master_data(&data, "map_6").unwrap();
        world.insert_resource(crate::resources::GameRng::new(4));
        let entities = world.query::<Entity>().iter(&world).collect::<Vec<_>>();
        for entity in entities {
            world.despawn(entity);
        }
        let mut map = Map::new(25, 9, Terrain::Plains, GridTopology::Square);
        for (position, terrain, owner) in [
            (pos(0, 0), Terrain::Capital, PlayerId(1)),
            (pos(24, 8), Terrain::Capital, PlayerId(2)),
            (pos(3, 2), Terrain::City, PlayerId(2)),
        ] {
            map.set_terrain(position.x, position.y, terrain).unwrap();
            world.spawn((position, Property::new(terrain, Some(owner), 200)));
        }
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(SquadManager::default());
        world.insert_resource(UnitOperationRegistry::default());
        let mut ai = crate::ai::PlayerAiSettings::new();
        ai.set_version(PlayerId(1), crate::ai::AiVersion::V4);
        world.insert_resource(ai);
        for player in &mut world.resource_mut::<crate::resources::Players>().0 {
            player.funds = 0;
        }
        let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
        let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(2, 2));
        let acted = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(0, 0));
        spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(24, 8));
        assign(&mut world, tank, MissionType::Defense, pos(3, 2));
        assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
        world.get_mut::<ActionCompleted>(acted).unwrap().0 = true;
        world.init_resource::<crate::ai::runtime::state::AiActionCooldown>();
        world
            .resource_mut::<crate::ai::runtime::state::AiActionCooldown>()
            .0
            .insert(acted);
        world.resource_mut::<WaitProbe>().settings = WaitProbeSettings {
            enabled: mode != 0,
            ..Default::default()
        };
        if mode == 2 {
            world
                .resource_mut::<WaitProbe>()
                .settings
                .units
                .insert(tank.to_bits());
        }
        let mut moved = EventCursor::<MoveUnitCommand>::default();
        let mut attacked = EventCursor::<AttackUnitCommand>::default();
        let mut captured = EventCursor::<CapturePropertyCommand>::default();
        let mut waited = EventCursor::<WaitUnitCommand>::default();
        let mut trace = Vec::new();
        let mut ended = false;
        for _ in 0..20 {
            let action = crate::ai::execute_ai_turn(&mut world, PlayerId(1));
            let commands = json!({
                "move":moved.read(world.resource::<Events<MoveUnitCommand>>()).map(|e|(e.unit_entity.to_bits(),e.target_x,e.target_y)).collect::<Vec<_>>(),
                "attack":attacked.read(world.resource::<Events<AttackUnitCommand>>()).map(|e|(e.attacker_entity.to_bits(),e.defender_entity.to_bits())).collect::<Vec<_>>(),
                "capture":captured.read(world.resource::<Events<CapturePropertyCommand>>()).map(|e|e.unit_entity.to_bits()).collect::<Vec<_>>(),
                "wait":waited.read(world.resource::<Events<WaitUnitCommand>>()).map(|e|e.unit_entity.to_bits()).collect::<Vec<_>>()});
            schedule.run(&mut world);
            let encoded = crate::serialize::export_save_data(&mut world, "probe_fixture").unwrap();
            let payload = BASE64_STANDARD
                .decode(encoded.split('.').nth(1).unwrap())
                .unwrap();
            let mut state: serde_json::Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(state["units"].as_array().unwrap().len(), 4);
            // 集合の列挙順だけを正規化し、乱数状態やユニットの値は比較から除かない。
            for field in [
                "units",
                "properties",
                "ai_action_cooldown",
                "ai_production_cooldown",
            ] {
                if let Some(values) = state[field].as_array_mut() {
                    values.sort_by_key(serde_json::Value::to_string);
                }
            }
            trace.push(json!({"action":action,"commands":commands,"state":state}));
            if action.is_none() {
                ended = true;
                break;
            }
        }
        assert!(ended, "手番が正常に終わる");
        runs.push(trace);
    }
    assert_eq!(
        runs[0], runs[1],
        "プローブONが発行順・actor・引数・盤面を変更しない"
    );
    assert_eq!(runs[0], runs[2], "詳細絞込も判断を変更しない");
}

#[test]
fn wait_probe_does_not_reuse_a_choice_after_another_command_or_turn() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    for change_turn in [false, true] {
        let mut world = world(true);
        let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
        assign(&mut world, tank, MissionType::Defense, pos(3, 2));
        crate::ai::wait_probe::begin_step(&mut world);
        let (_, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        if change_turn {
            world.resource_mut::<MatchState>().current_turn_number.0 += 1;
        } else {
            // 同一actor・同一位置でも、間に別命令を発行したら旧評価は使わない。
            crate::ai::execute_ai_command(
                &mut world,
                tank,
                AiCommand::Capture {
                    target_pos: pos(3, 2),
                },
            );
        }
        crate::ai::execute_ai_command(&mut world, tank, command);
        let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
        assert_eq!(probe["selection_stage"], "untraced");
    }
}

#[test]
fn different_issue_stage_does_not_reuse_a_previous_choice() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(3, 2));
    assign(&mut world, tank, MissionType::Defense, pos(3, 2));
    crate::ai::wait_probe::begin_step(&mut world);
    let (_, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    crate::ai::wait_probe::stage(&mut world, "late_production_relief");
    crate::ai::execute_ai_command(&mut world, tank, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    assert_eq!(probe["selection_stage"], "untraced");
    assert_eq!(probe["runtime_stage"], "late_production_relief");
}
