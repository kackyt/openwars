//! 火力支援計画の単体テスト。map_42 後攻（P2）側を模した盤面で期待値を固定する。
//!
//! 盤面（x = 1..=9 の 9 列、距離はマンハッタン距離）:
//! - `y = 3..=6`: 自陣側 `home`（`y = 5` が前線都市の列、`y = 6` が平地）
//! - `y = 7`: 陣地線 `L`（森）
//! - `y = 8`: 敵側の列 `E`（平地）

use super::*;

fn cell(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

fn manhattan(a: GridPosition, b: GridPosition) -> u32 {
    (a.x.abs_diff(b.x) + a.y.abs_diff(b.y)) as u32
}

fn row(y: usize) -> Vec<GridPosition> {
    (1..=9).map(|x| cell(x, y)).collect()
}

/// 指定したマスの砲座候補を探す。
fn site_at(sites: &[BatterySite], pos: GridPosition) -> Option<&BatterySite> {
    sites.iter().find(|site| site.position == pos)
}

const ROCKETS: FireRange = FireRange { min: 2, max: 3 };
const HEAVY_SP_GUN: FireRange = FireRange { min: 3, max: 5 };

struct Fixture {
    line: Vec<GridPosition>,
    enemy_side: Vec<GridPosition>,
    home: HashSet<GridPosition>,
}

impl Fixture {
    fn map_42_p2() -> Self {
        Self {
            line: row(7),
            enemy_side: row(8),
            home: (3..=6).flat_map(row).collect(),
        }
    }

    fn sites(&self, range: FireRange) -> Vec<BatterySite> {
        battery_sites(
            &FireSupportInput {
                line: &self.line,
                enemy_side: &self.enemy_side,
                home: &self.home,
                distance: manhattan,
            },
            range,
        )
    }
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn rockets_use_rows_4_to_6_and_never_the_capital_row() {
    let sites = Fixture::map_42_p2().sites(ROCKETS);
    let positions: Vec<_> = sites.iter().map(|site| site.position).collect();
    // y = 3 は L から 4 マス以上離れていて射程外。y = 4..=6 の 27 マスが候補になる。
    let expected: Vec<_> = (4..=6).flat_map(row).collect();
    assert_eq!(
        positions, expected,
        "砲座は (y, x) の昇順で y = 4..=6 の全マス"
    );
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn front_city_outside_production_range_covers_forest_and_plain() {
    let sites = Fixture::map_42_p2().sites(ROCKETS);
    // 前線都市 (3,5): 森 (2..=4, 7) と平地 (3, 8) を撃てる。
    let site = site_at(&sites, cell(3, 5)).expect("(3,5) は砲座候補");
    assert_eq!(
        site.covered,
        vec![cell(2, 7), cell(3, 7), cell(4, 7), cell(3, 8)]
    );
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn plain_behind_the_line_is_a_site_because_it_does_not_touch_the_enemy_side() {
    let sites = Fixture::map_42_p2().sites(ROCKETS);
    // (5,6) は L に隣接するが E には隣接しない。真正面の森 (5,7) は距離 1 で最小射程の内側。
    let site = site_at(&sites, cell(5, 6)).expect("(5,6) は砲座候補");
    assert_eq!(
        site.covered,
        vec![
            cell(3, 7),
            cell(4, 7),
            cell(6, 7),
            cell(7, 7),
            cell(4, 8),
            cell(5, 8),
            cell(6, 8),
        ]
    );
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn line_cells_in_home_are_excluded_because_they_touch_the_enemy_side() {
    let mut fixture = Fixture::map_42_p2();
    // home に陣地線が含まれていても、E に隣接するので砲座にはならない。
    fixture.home.extend(row(7));
    let sites = fixture.sites(ROCKETS);
    assert!(sites.iter().all(|site| site.position.y != 7));
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn minimum_range_dead_zone_moves_heavy_guns_back() {
    let sites = Fixture::map_42_p2().sites(HEAVY_SP_GUN);
    // 射程 3..=5 では y = 3 からも森 (x, 7) を撃てる（距離 4）。
    let capital_row = site_at(&sites, cell(5, 3)).expect("(5,3) は重自走砲の砲座候補");
    assert!(capital_row.covered.contains(&cell(5, 7)));
    // (5,6) からは真正面の森 (5,7)（距離 1）と平地 (5,8)（距離 2）は最小射程の内側で撃てない。
    let behind_line = site_at(&sites, cell(5, 6)).expect("(5,6) も候補");
    assert!(!behind_line.covered.contains(&cell(5, 7)));
    assert!(!behind_line.covered.contains(&cell(5, 8)));
    assert!(behind_line.covered.contains(&cell(7, 7)));
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn no_line_means_no_sites() {
    let home: HashSet<_> = (3..=6).flat_map(row).collect();
    let sites = battery_sites(
        &FireSupportInput {
            line: &[],
            enemy_side: &[],
            home: &home,
            distance: manhattan,
        },
        ROCKETS,
    );
    assert!(sites.is_empty(), "陣地線がないマップでは何も返さない");
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn purchase_deadline_matches_the_map_42_rocket_table() {
    // 敵が T5 に着く森を、1 手番先の砲座から撃つ: T3 に買えば T4 に布陣、T5 に撃てる。
    assert_eq!(purchase_deadline(5, 1), Some(3));
    // 生産施設そのものが砲座なら移動は 0 手番: T4 に買えば T5 に撃てる。
    assert_eq!(purchase_deadline(5, 0), Some(4));
    // 遠い施設（2 手番）なら 1 手番早く買う必要がある。
    assert_eq!(purchase_deadline(5, 2), Some(2));
}

#[test]
#[ignore = "Step 1 実装待ち"]
fn purchase_deadline_is_none_when_it_is_already_too_late() {
    assert_eq!(purchase_deadline(1, 1), None);
    assert_eq!(purchase_deadline(2, 1), Some(0));
}

#[test]
fn fire_range_includes_both_ends() {
    assert!(!ROCKETS.contains(1));
    assert!(ROCKETS.contains(2));
    assert!(ROCKETS.contains(3));
    assert!(!ROCKETS.contains(4));
}
