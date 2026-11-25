use super::{HoldLineSelection, HoldLineTile, hold_line_diagnostics_for_player, select_hold_line};
use crate::components::GridPosition;

#[test]
fn hold_line_trace_uses_turn_start_and_extended_v4_only() {
    use crate::ai::ai_version::{AiVersion, PlayerAiSettings};
    use crate::ai::code_path::CodePathRegistry;
    use crate::components::{Faction, PlayerId};
    use crate::resources::{GridTopology, UnitRegistry, UnitType, master_data::MasterDataRegistry};
    use crate::setup::initialize_world_from_master_data_with_topology;

    let registry = MasterDataRegistry::load().unwrap();
    let (mut world, _) =
        initialize_world_from_master_data_with_topology(&registry, "map_42", GridTopology::Hex)
            .unwrap();
    let mut settings = PlayerAiSettings::default();
    settings.set_version(PlayerId(1), AiVersion::V200);
    settings.set_version(PlayerId(2), AiVersion::V4);
    world.insert_resource(settings);
    let position = GridPosition { x: 4, y: 7 };
    // 中央の森に実ユニットがいれば、首都の生産を待たず到着0と記録する。
    let stats = world.resource::<UnitRegistry>().0[&UnitType::Infantry].clone();
    world.spawn((position, Faction(PlayerId(2)), stats));

    let trace = hold_line_diagnostics_for_player(&world, PlayerId(2)).unwrap();
    assert_eq!(trace["moves_first"], false);
    let forest = trace["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tile| tile["x"] == 4 && tile["y"] == 7)
        .unwrap();
    assert_eq!(forest["ours"], 0);
    assert_eq!(forest["defense"], 20);
    assert!(hold_line_diagnostics_for_player(&world, PlayerId(1)).is_none());
    assert!(!world.contains_resource::<CodePathRegistry>());

    let (mut standard, _) =
        initialize_world_from_master_data_with_topology(&registry, "map_6", GridTopology::Hex)
            .unwrap();
    let mut settings = PlayerAiSettings::default();
    settings.set_version(PlayerId(2), AiVersion::V4);
    standard.insert_resource(settings);
    assert!(hold_line_diagnostics_for_player(&standard, PlayerId(2)).is_none());
}

fn pos(x: usize) -> GridPosition {
    GridPosition { x, y: 0 }
}

fn tile(x: usize, ours: u32, theirs: u32, defense: u32) -> HoldLineTile {
    HoldLineTile {
        pos: pos(x),
        ours: Some(ours),
        theirs: Some(theirs),
        defense,
    }
}

fn select(
    tiles: &[HoldLineTile],
    edges: &[(usize, usize)],
    enemies: &[GridPosition],
    moves_first: bool,
) -> HoldLineSelection {
    select_hold_line(
        tiles,
        |from| {
            edges
                .iter()
                .filter_map(|&(a, b)| match from.x {
                    x if x == a => Some(pos(b)),
                    x if x == b => Some(pos(a)),
                    _ => None,
                })
                .collect()
        },
        &[pos(0)],
        enemies,
        moves_first,
    )
}

#[test]
fn rear_player_tie_is_not_first_arrival() {
    let tiles = [
        tile(0, 0, 3, 20),
        tile(1, 1, 2, 5),
        tile(2, 2, 2, 20),
        tile(3, 3, 0, 5),
    ];
    let edges = [(0, 1), (1, 2), (2, 3)];
    let rear = select(&tiles, &edges, &[pos(3)], false);
    assert_eq!(rear.strict_line(), Some(&[pos(1)][..]));
    assert_eq!(rear.extended_line(), Some(&[pos(2)][..]));
    let first = select(&tiles, &edges, &[pos(3)], true);
    assert_eq!(first.strict_line(), Some(&[pos(2)][..]));
}

#[test]
fn two_bridges_require_both_paths_to_be_cut() {
    let tiles = [
        tile(0, 0, 3, 20),
        tile(1, 1, 2, 5),
        tile(2, 1, 2, 5),
        tile(3, 3, 0, 5),
    ];
    let edges = [(0, 1), (1, 3), (0, 2), (2, 3)];
    let decision = select(&tiles, &edges, &[pos(3)], false);
    assert_eq!(decision.strict_line(), Some(&[pos(1), pos(2)][..]));
}

#[test]
fn disconnected_island_is_not_a_line() {
    let tiles = [tile(0, 0, 2, 20), tile(1, 2, 0, 20)];
    let decision = select(&tiles, &[], &[pos(1)], false);
    assert!(decision.strict_line().is_none());
    assert!(decision.extended_line().is_none());
}

#[test]
fn enemy_already_past_layer_invalidates_it() {
    let tiles = [
        tile(0, 0, 1, 20),
        tile(1, 1, 2, 5),
        tile(2, 2, 2, 20),
        tile(3, 3, 0, 5),
        tile(4, 3, 0, 20),
        tile(5, 2, 1, 5),
    ];
    // マス4は敵が先に入った自軍物件。マス2を封鎖しても、マス5から到達される。
    let edges = [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)];
    let decision = select_hold_line(
        &tiles,
        |from| {
            edges
                .iter()
                .filter_map(|&(a, b)| match from.x {
                    x if x == a => Some(pos(b)),
                    x if x == b => Some(pos(a)),
                    _ => None,
                })
                .collect()
        },
        &[pos(0), pos(4)],
        &[pos(3), pos(5)],
        false,
    );
    assert!(decision.strict_line().is_none());
    assert!(decision.extended_line().is_none());
}

#[test]
fn enemy_occupied_layer_is_not_a_holdable_cut() {
    let tiles = [tile(0, 0, 2, 20), tile(1, 1, 2, 5), tile(2, 2, 0, 20)];
    let decision = select(&tiles, &[(0, 1), (1, 2)], &[pos(2)], false);
    // 森をグラフから消すと敵は消えるが、実際には先にその敵を排除する必要がある。
    assert_eq!(decision.extended_line(), Some(&[pos(1)][..]));
}

#[test]
fn unreachable_layer_is_not_a_holdable_cut() {
    let mut tiles = [
        tile(0, 0, 3, 20),
        tile(1, 1, 2, 5),
        tile(2, 2, 2, 20),
        tile(3, 3, 0, 5),
    ];
    tiles[2].ours = None;
    let decision = select(&tiles, &[(0, 1), (1, 2), (2, 3)], &[pos(3)], false);
    assert_eq!(decision.extended_line(), Some(&[pos(1)][..]));
}

#[test]
fn compares_mean_defense_and_is_order_independent() {
    let mut tiles = vec![tile(0, 0, 3, 20), tile(1, 2, 2, 20), tile(2, 3, 0, 5)];
    let mut edges = vec![(1, 2)];
    for x in 3..8 {
        tiles.push(tile(x, 1, 2, 5));
        edges.extend([(0, x), (x, 1)]);
    }
    let expected = Some(&[pos(1)][..]);
    assert_eq!(
        select(&tiles, &edges, &[pos(2)], false).extended_line(),
        expected
    );
    tiles.reverse();
    edges.reverse();
    assert_eq!(
        select(&tiles, &edges, &[pos(2)], false).extended_line(),
        expected
    );
    tiles
        .iter_mut()
        .find(|tile| tile.pos == pos(1))
        .unwrap()
        .defense = 5;
    assert_eq!(
        select(&tiles, &edges, &[pos(2)], false).extended_line(),
        expected
    );
}
