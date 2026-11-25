use super::*;

fn pos(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

fn occupant(id: u32, kind: UnitType) -> OccupantInfo {
    OccupantInfo {
        player_id: PlayerId(id),
        is_transport: false,
        unit_type: kind,
        loadable_types: vec![],
        free_slots: 0,
    }
}

#[test]
fn hold_line_rejects_moving_a_guard_that_opens_a_worse_threat() {
    let source = pos(0, 0);
    let destination = pos(1, 0);
    let actual = Occupancy::from_iter([(xy(source), occupant(2, UnitType::Infantry))]);
    let placement = Placement {
        unit: Entity::from_raw(1),
        from: source,
        to: destination,
        defense: 20,
        cost: 1,
    };
    // 予定砲座を守れても、元マスを空けて実砲座を危険にさらす手は採らない。
    let risk = |occ: &Occupancy| {
        [
            usize::from(!occ.contains_key(&xy(source))),
            0,
            usize::from(!occ.contains_key(&xy(destination))),
            0,
        ]
    };
    assert!(choose(&actual, &actual, [placement], risk).is_none());
}

#[test]
fn hold_line_counts_only_a_positive_marginal_placement() {
    let source = pos(0, 0);
    let destination = pos(1, 0);
    let actual = Occupancy::from_iter([(xy(source), occupant(2, UnitType::Infantry))]);
    let placement = Placement {
        unit: Entity::from_raw(1),
        from: source,
        to: destination,
        defense: 20,
        cost: 1,
    };
    // 内側へ抜けた敵を除けなくても、後続が砲座に到達する組が減れば採る。
    let chosen = choose(&actual, &actual, [placement], |occ| {
        let outside = usize::from(!occ.contains_key(&xy(destination)));
        [1 + outside, 1 + outside, 0, 0]
    })
    .unwrap();
    assert_eq!(chosen.placement.to, destination);
    assert_eq!((chosen.before, chosen.after), ([2, 2, 0, 0], [1, 1, 0, 0]));
    assert!(choose(&actual, &actual, [placement], |_| [1, 1, 0, 0]).is_none());
}

#[test]
fn hold_line_planned_battery_replaces_a_friendly_occupant_only() {
    let seat = pos(0, 0);
    let mut actual = Occupancy::from_iter([(xy(seat), occupant(2, UnitType::Infantry))]);
    let simulated = place_battery(&actual, seat, PlayerId(2), UnitType::Rockets).unwrap();
    assert_eq!(simulated.len(), 1);
    assert_eq!(simulated[&xy(seat)].unit_type, UnitType::Rockets);
    assert_eq!(actual[&xy(seat)].unit_type, UnitType::Infantry);
    actual.insert(xy(seat), occupant(1, UnitType::Tank));
    assert!(place_battery(&actual, seat, PlayerId(2), UnitType::Rockets).is_none());
}

#[test]
fn hold_line_projection_uses_enemy_zoc_to_limit_real_stops() {
    use crate::resources::GridTopology;
    let master = MasterDataRegistry::load().unwrap();
    let map = Map::new(5, 2, Terrain::Plains, GridTopology::Square);
    let source = pos(0, 0);
    let stopped = pos(2, 0);
    let shooting_tile = pos(3, 0);
    let guard = pos(2, 1);
    let tank = Mover {
        position: source,
        owner: PlayerId(1),
        kind: UnitType::Tank,
        movement: MovementType::Tank,
        mp: 4,
        fuel: 10,
    };
    let mut occ = Occupancy::from_iter([(xy(source), occupant(1, UnitType::Tank))]);
    assert!(
        tank.stops(&map, &master, &occ)
            .contains_key(&xy(shooting_tile))
    );
    occ.insert(xy(guard), occupant(2, UnitType::Infantry));
    let stops = tank.stops(&map, &master, &occ);
    assert!(stops.contains_key(&xy(stopped)));
    assert!(!stops.contains_key(&xy(shooting_tile)));
}
