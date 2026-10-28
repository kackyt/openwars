//! 一時調査用（コミットしない）: map_20 で V4 の手番ごとの盤面を書き出す。
use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;

fn letter(t: UnitType) -> char {
    match t {
        UnitType::Infantry => 'i',
        UnitType::Mech => 'm',
        UnitType::MdTank => 'd',
        UnitType::TankZ => 'z',
        UnitType::Tank => 't',
        UnitType::HeavySpGun => 'h',
        UnitType::LightSpGun => 'l',
        UnitType::Rockets => 'r',
        UnitType::Artillery => 'a',
        UnitType::Recon => 'v',
        _ => '?',
    }
}

#[test]
#[ignore]
fn map20_board_dump() {
    let map_name = std::env::var("PROBE_MAP").unwrap_or_else(|_| "map_20".into());
    let seed: u64 = std::env::var("PROBE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let turns: u32 = std::env::var("PROBE_TURNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let md = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &md,
        &map_name,
        if std::env::var("PROBE_HEX").is_ok() { GridTopology::Hex } else { GridTopology::Square },
    )
    .unwrap();
    world.insert_resource(GameRng::new(seed));
    let mut s = PlayerAiSettings::default();
    let v4_first = std::env::var("PROBE_V4_FIRST").is_ok();
    s.set_version(PlayerId(1), if v4_first { AiVersion::V4 } else { AiVersion::V200 });
    s.set_version(PlayerId(2), if v4_first { AiVersion::V200 } else { AiVersion::V4 });
    world.insert_resource(s);
    loop {
        let (turn, active, over) = {
            let ms = world.resource::<MatchState>();
            let ps = world.resource::<Players>();
            (
                ms.current_turn_number.0,
                ps.0[ms.active_player_index.0].id,
                ms.game_over.is_some(),
            )
        };
        if over || turn > turns {
            break;
        }
        let mut steps = 0;
        for _ in 0..400 {
            let a = execute_ai_turn(&mut world, active);
            schedule.run(&mut world);
            steps += 1;
            if std::env::var("PROBE_ACTIONS").is_ok() { eprintln!("T{turn} P{:?} {:?}", active, a); }
            if a.is_none() {
                break;
            }
        }
        eprintln!("T{turn} {:?} steps={steps}", active);
        if active != PlayerId(2) {
            continue;
        }
        let map = world.resource::<Map>().clone();
        let mut grid: Vec<Vec<String>> = (0..map.height)
            .map(|y| {
                (0..map.width)
                    .map(|x| {
                        let c = match map.get_terrain(x, y) {
                            Some(Terrain::Mountain) => "^",
                            Some(Terrain::Sea) => " ",
                            Some(Terrain::River) => "~",
                            Some(Terrain::Road) | Some(Terrain::Bridge) => "=",
                            Some(Terrain::Forest) => "f",
                            _ => ".",
                        };
                        format!("{c}  ")
                    })
                    .collect()
            })
            .collect();
        for (pos, prop) in world.query::<(&GridPosition, &Property)>().iter(&world) {
            let o = match prop.owner_id {
                Some(PlayerId(1)) => "1",
                Some(PlayerId(2)) => "2",
                _ => "o",
            };
            grid[pos.y][pos.x] = format!("{o}  ");
        }
        for (pos, f, st, hp) in world
            .query::<(&GridPosition, &Faction, &UnitStats, &Health)>()
            .iter(&world)
        {
            if pos.y >= map.height || pos.x >= map.width {
                continue;
            }
            let mut c = letter(st.unit_type);
            if f.0 == PlayerId(2) {
                c = c.to_ascii_uppercase();
            }
            let h = (hp.current + 9) / 10;
            grid[pos.y][pos.x] = format!(
                "{c}{:<2}",
                if h >= 10 {
                    "".to_string()
                } else {
                    h.to_string()
                }
            );
        }
        println!("=== T{turn} after P2 (大文字=V4) ===");
        print!("   ");
        for x in 0..map.width {
            print!("{:<3}", x % 100);
        }
        println!();
        for (y, row) in grid.iter().enumerate() {
            println!("{y:2} {}", row.concat());
        }
    }
}
