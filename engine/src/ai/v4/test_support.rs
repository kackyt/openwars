use crate::ai::v4::*;
use crate::resources::GridTopology;

/// テスト用に、DAG orderを読むEntityを具体Squadへ接続する。
///
/// 本番と同じく `Entity -> UnitOperationRegistry -> SquadId -> route order` を
/// 通るため、Entityを直接キーにした旧来のfixtureを再導入しない。
pub(in crate::ai::v4) fn assign_route_squad(
    world: &mut World,
    player: PlayerId,
    mission: MissionType,
    members: &[Entity],
) -> SquadId {
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap_or_default();
    let mut squad = manager.create_owned_squad(mission, player);
    squad.members.extend(members.iter().copied());
    let squad_id = squad.id;
    world.insert_resource(manager);

    let mut assignments = world
        .remove_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .unwrap_or_default();
    for entity in members {
        assignments.assign(
            *entity,
            crate::ai::operation_assignment::UnitOperationAssignment {
                owner: crate::ai::operation_assignment::OperationOwner::TacticalSquad {
                    player_id: player,
                    squad_id,
                },
                squad_id: Some(squad_id),
                role: crate::ai::operation_assignment::OperationUnitRole::Member,
                assigned_turn: 0,
            },
        );
    }
    world.insert_resource(assignments);
    squad_id
}

pub(in crate::ai::v4) fn flat_map(width: usize, height: usize) -> Map {
    Map {
        width,
        height,
        tiles: vec![Terrain::Plains; width * height],
        topology: GridTopology::Square,
    }
}

pub(in crate::ai::v4) fn pos(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

pub(in crate::ai::v4) fn capture_objective(anchor: GridPosition) -> Vec<CampaignPlanningObjective> {
    vec![CampaignPlanningObjective {
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor,
        objective_properties: vec![anchor],
        capture_eta: None,
        required_capture_survivors: 0,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: anchor,
        execution_authorized: true,
    }]
}

/// テスト用のユニット諸元。
pub(in crate::ai::v4) fn stats(unit_type: UnitType, cost: u32) -> UnitStats {
    let master = MasterDataRegistry::load().unwrap();
    let weapon_stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            unit_type.as_str().to_owned(),
        ))
        .unwrap();
    UnitStats {
        unit_type,
        cost,
        max_ammo1: weapon_stats.max_ammo1,
        max_ammo2: weapon_stats.max_ammo2,
        min_range: weapon_stats.min_range,
        max_range: weapon_stats.max_range,
        ..UnitStats::mock()
    }
}

/// 揚陸判定用のマップを組み立てる。
///
/// 横一列のレーンを、左の陸地／中央の海／右の陸地に区切る。
/// `landing` に地形を与えると右岸の入口 (x=6, y=1) をその地形に差し替えられる。
/// 例: `Terrain::Shoal` を与えれば艦船が接岸できる揚陸地点になり、
/// `None` のままなら海から陸へ乗り上げる手段が無いマップになる。
///
/// レイアウト（各行 x=0..8 共通）:
///   Plains Port | Sea Sea Sea Sea | (landing) Plains Plains
pub(in crate::ai::v4) fn strait_map(landing: Option<Terrain>) -> Map {
    let width = 9;
    let height = 3;
    let mut tiles = vec![Terrain::Sea; width * height];
    for y in 0..height {
        for x in 0..width {
            let terrain = match x {
                0 => Terrain::Plains,
                1 => Terrain::Port,
                6 => landing.unwrap_or(Terrain::Sea),
                7..=8 => Terrain::Plains,
                _ => Terrain::Sea,
            };
            tiles[y * width + x] = terrain;
        }
    }
    Map {
        width,
        height,
        tiles,
        topology: GridTopology::Square,
    }
}

/// テスト用の作戦。枠の充足状況だけを見たいので敵情報は空にしておく。
pub(in crate::ai::v4) fn operation(
    kind: OperationKind,
    slots: OperationSlots,
    filled: OperationSlots,
) -> Operation {
    Operation {
        kind,
        island_id: None,
        operation_id: None,
        anchor: pos(0, 0),
        staging_anchor: pos(0, 0),
        execution_authorized: true,
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        objective_properties: vec![pos(0, 0)],
        property_control: None,
        property_controls: Vec::new(),
        capture_lane_targets: Vec::new(),
        planned_capture_units: Vec::new(),
        threat_horizon: 0,
        facts: OperationFacts::default(),
        slots,
        filled,
        unreachable_threats: Vec::new(),
        reachable_threats: Vec::new(),
        expected_reinforcements: Vec::new(),
        unavoidable_reinforcements: Vec::new(),
        reinforcement_contingencies: Vec::new(),
        stress_reinforcement_funds: 0,
        contingency_reserve_funds: 0,
    }
}

/// 海峡マップ上に、母港の輸送艦 1 隻だけを置いた盤面を作る。
///
/// 対岸 (8,1) を獲りにいく作戦から見て、この輸送艦は
/// 「距離では母港側が最寄り」だが「積荷を対岸へ届けられる」という位置関係になる。
pub(in crate::ai::v4) fn strait_scan() -> BoardScan {
    let infantry = UnitStats {
        can_capture: true,
        max_movement: 3,
        ..stats(UnitType::Infantry, 1000)
    };
    let lander = UnitStats {
        movement_type: MovementType::Ship,
        max_movement: 6,
        max_cargo: 2,
        ..stats(UnitType::Lander, 12000)
    };

    BoardScan {
        map: strait_map(Some(Terrain::Shoal)).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: DamageChart::new().into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 20000,
        capital_position: None,
        free_facilities: vec![(pos(1, 1), Terrain::Port)],
        production_facilities: vec![(pos(1, 1), Terrain::Port)],
        available_types: vec![
            (UnitType::Infantry, infantry),
            (UnitType::Lander, lander.clone()),
        ],
        // 母港 (1,1) に停泊したままの輸送艦。空き搭載スロット 2。
        my_units: vec![UnitSnapshot {
            entity: None,
            pos: pos(1, 1),
            stats: lander,
            hp: 100,
            free_cargo: 2,
        }],
        enemy_units: Vec::new(),
        property_entities: HashMap::default(),
        owned_airport_count: 0,
        open_properties: vec![pos(8, 1)],
        neutral_properties: vec![pos(8, 1)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 1000,
        campaign_objectives: Vec::new(),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    }
}

/// 平地マップに工場 3 基と未取得拠点を置いた、生産ループ検証用の盤面。
///
/// 空き施設を複数持たせることで「同一ターン内に複数施設へ発注が飛ぶ」状況を作り、
/// その内訳がトレースに残るかを確かめられるようにする。
pub(in crate::ai::v4) fn multi_factory_scan() -> BoardScan {
    let infantry = UnitStats {
        can_capture: true,
        max_movement: 3,
        ..stats(UnitType::Infantry, 1000)
    };
    let tank = UnitStats {
        max_movement: 6,
        ..stats(UnitType::Tank, 7000)
    };

    BoardScan {
        map: flat_map(9, 5).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: DamageChart::new().into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 20000,
        capital_position: None,
        free_facilities: vec![
            (pos(1, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(1, 3), Terrain::Factory),
        ],
        production_facilities: vec![
            (pos(1, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(1, 3), Terrain::Factory),
        ],
        available_types: vec![(UnitType::Infantry, infantry), (UnitType::Tank, tank)],
        my_units: Vec::new(),
        enemy_units: Vec::new(),
        property_entities: HashMap::default(),
        owned_airport_count: 0,
        open_properties: vec![pos(6, 2)],
        neutral_properties: vec![pos(6, 2)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 1000,
        campaign_objectives: capture_objective(pos(6, 2)),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    }
}

/// 航空・地上の未対処脅威と工場3基を持つ、限界価値の統合テスト盤面。
pub(in crate::ai::v4) fn mixed_threat_multi_factory_scan() -> BoardScan {
    let infantry = UnitStats {
        can_capture: true,
        max_movement: 3,
        ..stats(UnitType::Infantry, 1000)
    };
    let anti_air = UnitStats {
        max_movement: 6,
        ..stats(UnitType::AntiAir, 8000)
    };
    let tank = UnitStats {
        max_movement: 6,
        ..stats(UnitType::Tank, 7000)
    };
    let mut damage_chart = DamageChart::new();
    damage_chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
    damage_chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
    damage_chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
    damage_chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 20);
    damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);

    BoardScan {
        map: flat_map(9, 5).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 17000,
        capital_position: None,
        free_facilities: vec![
            (pos(1, 0), Terrain::Factory),
            (pos(1, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(1, 3), Terrain::Factory),
        ],
        production_facilities: vec![
            (pos(1, 0), Terrain::Factory),
            (pos(1, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(1, 3), Terrain::Factory),
        ],
        available_types: vec![
            (UnitType::Infantry, infantry.clone()),
            (UnitType::AntiAir, anti_air),
            (UnitType::Tank, tank),
        ],
        my_units: Vec::new(),
        enemy_units: vec![
            UnitSnapshot {
                entity: Some(Entity::from_raw(101)),
                pos: pos(6, 1),
                stats: stats(UnitType::Bcopters, 8000),
                hp: 100,
                free_cargo: 0,
            },
            UnitSnapshot {
                entity: Some(Entity::from_raw(102)),
                pos: pos(6, 3),
                stats: infantry,
                hp: 100,
                free_cargo: 0,
            },
        ],
        property_entities: HashMap::default(),
        owned_airport_count: 0,
        open_properties: vec![pos(6, 2)],
        neutral_properties: vec![pos(6, 2)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 1000,
        campaign_objectives: capture_objective(pos(6, 2)),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    }
}
