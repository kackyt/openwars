//! 検証専用: 陣地線（地形の切断面）の空きマスへ、占領担当も含む歩兵を強制的に向かわせて保持させる。
//!
//! 「人間のように森の列を歩兵で埋め続ければ勝てるか」という効果の上限を測るための実験スイッチ。
//! `OPENWARS_V4_FORCE_LINE_HOLD=1` のときだけ有効で、未設定時は何もしない（既定の挙動は変わらない）。
//! 陣地線は座標を与えず、初期地形から選んだ切断面（[`super::terrain`]）をそのまま使う。
//! 割当は手番をまたいで保持し、届く手番はそのマスで止まり、届かない手番はそのマスへ近づく。

use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeMap;

/// 強制保持を有効にする環境変数。
const FORCE_ENV: &str = "OPENWARS_V4_FORCE_LINE_HOLD";

/// 強制保持の実験スイッチが有効か。
pub(crate) fn enabled() -> bool {
    std::env::var(FORCE_ENV).as_deref() == Ok("1")
}

/// 森マスへの出発合図: そのマスに敵地上部隊が入っているか、隣接しているマスだけを返す。
///
/// 人間の勝ち局では、敵が森の列に触れた手番に歩兵を森へ入れている。最悪の到着手番で見積もると
/// 占領中の敵まで数えて早すぎるため、盤面上で実際に接したことだけを合図にする（調整用の定数は持たない）。
/// `enemies` は敵地上部隊の位置、`adjacent` は盤面の隣接関係。
pub(crate) fn contested_cells(
    cells: &[GridPosition],
    enemies: &HashSet<GridPosition>,
    adjacent: impl Fn(GridPosition) -> Vec<GridPosition>,
) -> HashSet<GridPosition> {
    cells
        .iter()
        .copied()
        .filter(|cell| {
            enemies.contains(cell) || adjacent(*cell).iter().any(|next| enemies.contains(next))
        })
        .collect()
}

/// 歩兵だけでなく対地間接部隊も配置する実験か。
pub(crate) fn with_batteries() -> bool {
    std::env::var("OPENWARS_V4_FORCE_LINE_BATTERY").as_deref() == Ok("1")
}

/// 自陣側で線に最も近い都市のうち、自軍が占領した都市だけを砲座候補にする。
/// 所有権を絞る前に距離を比べることで、未占領のときに後方の都市を砲座にしない。
pub(crate) fn forward_battery_sites(cities: &[(GridPosition, u32, bool)]) -> Vec<GridPosition> {
    let nearest = cities.iter().map(|(_, distance, _)| *distance).min();
    cities
        .iter()
        .filter(|(_, distance, owned)| *owned && Some(*distance) == nearest)
        .map(|(pos, _, _)| *pos)
        .collect()
}

/// 自陣側で陣地線に最も近い距離にある都市（前線都市）の集合を返す。
/// 所有・未所有を問わずに最小距離を決めることで、未占領の間も前線都市だけを選べる。
pub(crate) fn front_cities(cities: &[(GridPosition, u32)]) -> HashSet<GridPosition> {
    let Some(nearest) = cities.iter().map(|(_, distance)| *distance).min() else {
        return HashSet::default();
    };
    cities
        .iter()
        .filter(|(_, distance)| *distance == nearest)
        .map(|(pos, _)| *pos)
        .collect()
}

/// 強制保持時の占領目標順: 前線都市を先に並べ、それぞれ最寄りの歩兵が近い順に並べる。
pub(crate) fn sort_capture_targets_front_first(
    targets: &mut [GridPosition],
    front_cities: &HashSet<GridPosition>,
    nearest_infantry_distance: impl Fn(GridPosition) -> usize,
) {
    targets.sort_unstable_by_key(|target| {
        (
            !front_cities.contains(target),
            nearest_infantry_distance(*target),
            target.y,
            target.x,
        )
    });
}

/// 占領中の拠点上か（未所有かつ捕獲ポイントが最大値より減っている）。
pub(crate) fn is_capturing_in_progress(
    owner_id: Option<PlayerId>,
    player_id: PlayerId,
    capture_points: u32,
    max_capture_points: u32,
) -> bool {
    owner_id != Some(player_id) && capture_points < max_capture_points
}

/// 強制保持で陣地線への割当対象にしてよいか。
///
/// - 占領中の拠点上の兵は引き抜かない。
/// - すでに陣地線へ割当済みの兵は、次手番に占領任務を再付与されても維持する。
/// - 所有済み前線都市の上にいる兵（占領完了・前線生産）は線へ解放する。
/// - それ以外の場所で占領任務を持つ兵は、占領完了を優先し引き抜かない。
pub(crate) fn can_hold_line_in_forced_mode(
    capturing_in_progress: bool,
    already_assigned: bool,
    on_owned_front_city: bool,
    has_capture_duty: bool,
) -> bool {
    if capturing_in_progress {
        return false;
    }
    already_assigned || on_owned_front_city || !has_capture_duty
}

/// プレイヤーごとの「兵 → 保持マス」の割当。手番をまたいで同じ兵に同じマスを持たせ続ける。
#[derive(Resource, Default)]
pub(crate) struct ForcedLineHold {
    assignments: HashMap<PlayerId, BTreeMap<Entity, GridPosition>>,
}

/// 前回までの割当を読む。未記録なら空。
pub(crate) fn previous(world: &World, player: PlayerId) -> BTreeMap<Entity, GridPosition> {
    world
        .get_resource::<ForcedLineHold>()
        .and_then(|hold| hold.assignments.get(&player))
        .cloned()
        .unwrap_or_default()
}

/// 今回の割当を記録する。
pub(crate) fn store(
    world: &mut World,
    player: PlayerId,
    assignments: BTreeMap<Entity, GridPosition>,
) {
    world
        .get_resource_or_insert_with(ForcedLineHold::default)
        .assignments
        .insert(player, assignments);
}

/// 割当の入力。
pub(crate) struct AssignInput<'a> {
    /// 陣地線の全マス。
    pub(crate) line: &'a [GridPosition],
    /// 今は割り当てられないマス（敵がいる、保持対象外の味方がいる）。
    pub(crate) unavailable: &'a HashSet<GridPosition>,
    /// 新たに割り当ててよい兵（この手番に動ける歩兵）。
    pub(crate) pool: &'a [Entity],
}

/// 前回の割当を引き継ぎ、空いた陣地線マスへ近い兵から1対1で割り当てる。
///
/// - `retained(兵)` が偽の兵（死亡・回復優先など保持資格を失った兵）の割当は外す。
/// - 割当マスが使えなくなった（敵が入った等）兵は割当を外し、改めて候補に戻す。
/// - `cost(兵, マス)` は到達までの（ターン数, 消費移動力）。到達不能なら `None`。
///   全組から最小のものを順に確定する（同値は兵・マスの順で決定的に選ぶ）。
pub(crate) fn assign(
    previous: &BTreeMap<Entity, GridPosition>,
    input: &AssignInput,
    retained: impl Fn(Entity) -> bool,
    mut cost: impl FnMut(Entity, GridPosition) -> Option<(u32, u32)>,
) -> BTreeMap<Entity, GridPosition> {
    let mut result: BTreeMap<Entity, GridPosition> = previous
        .iter()
        .filter(|(unit, cell)| {
            retained(**unit) && input.line.contains(cell) && !input.unavailable.contains(cell)
        })
        .map(|(unit, cell)| (*unit, *cell))
        .collect();
    let mut taken: HashSet<GridPosition> = result.values().copied().collect();
    let open: Vec<GridPosition> = input
        .line
        .iter()
        .copied()
        .filter(|cell| !taken.contains(cell) && !input.unavailable.contains(cell))
        .collect();
    let mut pairs = Vec::new();
    for &unit in input.pool.iter().filter(|unit| !result.contains_key(unit)) {
        for &cell in &open {
            if let Some(distance) = cost(unit, cell) {
                pairs.push((distance, unit, (cell.y, cell.x), cell));
            }
        }
    }
    // 近い組から確定する。同じ兵・同じマスは一度だけ使う。
    pairs.sort_unstable_by_key(|(distance, unit, key, _)| (*distance, *unit, *key));
    for (_, unit, _, cell) in pairs {
        if result.contains_key(&unit) || taken.contains(&cell) {
            continue;
        }
        taken.insert(cell);
        result.insert(unit, cell);
    }
    result
}

/// 前線都市守備の試験スイッチ（`OPENWARS_V4_FORCE_CITY_GUARD=1`）。強制保持が有効なときだけ使う。
pub(crate) fn city_guard_enabled() -> bool {
    std::env::var("OPENWARS_V4_FORCE_CITY_GUARD").as_deref() == Ok("1")
}

/// 奪われた前線都市の上の敵を優先攻撃する試験スイッチ（`OPENWARS_V4_FORCE_CITY_RECAPTURE=1`）。
/// 強制保持が有効なときだけ使う。空いた都市の占領は既存の占領計画に任せる。
pub(crate) fn city_recapture_enabled() -> bool {
    std::env::var("OPENWARS_V4_FORCE_CITY_RECAPTURE").as_deref() == Ok("1")
}

/// 守備上重要な都市にいる兵を都市担当にする。
///
/// `garrisons` は自軍の非生産前線都市の上にいる保持候補の兵と、その都市。
/// `threatened(兵, 都市)` は、その守備兵の占有だけを外したときに敵地上部隊が次の手番に入れるか。
/// 森か都市かの二択にはせず、守れていて空けると危険な都市だけを担当として残す。
pub(crate) fn city_guards(
    garrisons: &[(Entity, GridPosition)],
    mut threatened: impl FnMut(Entity, GridPosition) -> bool,
) -> BTreeMap<Entity, GridPosition> {
    garrisons
        .iter()
        .filter(|(unit, city)| threatened(*unit, *city))
        .map(|(unit, city)| (*unit, *city))
        .collect()
}

/// 奪還対象の前線都市: 敵が所有している都市と、自軍所有だが敵が上にいる都市。
///
/// `cities` は非生産前線都市の（位置, 所有者, 敵地上部隊が上にいるか）。中立都市は通常の
/// 占領順に任せ、敵のいない自軍都市は守備（[`city_guards`]）の対象なので含めない。
pub(crate) fn lost_cities(
    cities: &[(GridPosition, Option<PlayerId>, bool)],
    me: PlayerId,
) -> HashSet<GridPosition> {
    cities
        .iter()
        .filter(|(_, owner, enemy_on)| match owner {
            Some(owner) if *owner == me => *enemy_on,
            Some(_) => true,
            None => false,
        })
        .map(|(position, ..)| *position)
        .collect()
}

/// 都市担当と陣地線（森）担当を合わせた割当結果。
pub(crate) struct HoldPlan {
    /// 陣地線マスの担当。
    pub(crate) line: BTreeMap<Entity, GridPosition>,
    /// 都市担当（その場で射撃・待機する）。
    pub(crate) guards: BTreeMap<Entity, GridPosition>,
    /// 空いていて、届く兵もいない陣地線マス（診断用）。
    pub(crate) unfilled: Vec<GridPosition>,
}

/// 都市担当を除いた兵で陣地線を割り当てる。
/// 都市担当は前回の森の割当からも、新規候補からも外す。
pub(crate) fn assign_with_city_guards(
    previous: &BTreeMap<Entity, GridPosition>,
    input: &AssignInput,
    guards: BTreeMap<Entity, GridPosition>,
    retained: impl Fn(Entity) -> bool,
    cost: impl FnMut(Entity, GridPosition) -> Option<(u32, u32)>,
) -> HoldPlan {
    let pool = input
        .pool
        .iter()
        .copied()
        .filter(|unit| !guards.contains_key(unit))
        .collect::<Vec<_>>();
    let line = assign(
        previous,
        &AssignInput {
            line: input.line,
            unavailable: input.unavailable,
            pool: &pool,
        },
        |unit| !guards.contains_key(&unit) && retained(unit),
        cost,
    );
    let taken = line.values().copied().collect::<HashSet<_>>();
    let unfilled = input
        .line
        .iter()
        .copied()
        .filter(|cell| !taken.contains(cell) && !input.unavailable.contains(cell))
        .collect();
    HoldPlan {
        line,
        guards,
        unfilled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    fn entities(n: u32) -> Vec<Entity> {
        let mut world = World::new();
        (0..n).map(|_| world.spawn_empty().id()).collect()
    }

    #[test]
    fn 敵が接したマスだけ出発の合図を出す() {
        let line = [cell(1, 7), cell(2, 7), cell(3, 7)];
        // 一次元の隣接（x±1, 同じ y）で十分に判定を確かめられる。
        let adjacent = |c: GridPosition| {
            [c.x.wrapping_sub(1), c.x + 1]
                .into_iter()
                .map(|x| cell(x, c.y))
                .collect::<Vec<_>>()
        };
        // 敵が遠い間は合図なし。
        let far = HashSet::from_iter([cell(9, 7)]);
        assert!(contested_cells(&line, &far, adjacent).is_empty());
        // (4,7) の敵は (3,7) にだけ隣接する。(2,7) に入った敵はそのマスと両隣を合図にする。
        let near = HashSet::from_iter([cell(4, 7)]);
        assert_eq!(
            contested_cells(&line, &near, adjacent),
            HashSet::from_iter([cell(3, 7)])
        );
        let inside = HashSet::from_iter([cell(2, 7)]);
        assert_eq!(contested_cells(&line, &inside, adjacent).len(), 3);
    }

    #[test]
    fn 前線都市を取るまでは後方都市を砲座と見なさない() {
        let (front, rear) = (cell(3, 5), cell(3, 3));
        assert!(forward_battery_sites(&[(front, 2, false), (rear, 4, true)]).is_empty());
        assert_eq!(
            forward_battery_sites(&[(front, 2, true), (rear, 4, true)]),
            vec![front]
        );
    }

    #[test]
    fn 近い兵から空きマスへ1対1で割り当てる() {
        let units = entities(3);
        let line = [cell(1, 7), cell(2, 7)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        // 兵0はどちらも遠く、兵1は(1,7)に、兵2は(2,7)に近い。
        let result = assign(
            &BTreeMap::new(),
            &input,
            |_| true,
            |unit, c| {
                Some(match (units.iter().position(|u| *u == unit)?, c.x) {
                    (0, _) => (3, 0),
                    (1, 1) | (2, 2) => (1, 0),
                    _ => (2, 0),
                })
            },
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[&units[1]], cell(1, 7));
        assert_eq!(result[&units[2]], cell(2, 7));
    }

    #[test]
    fn 前回の割当を引き継ぎ_外れた兵と使えないマスは解放する() {
        let units = entities(3);
        let line = [cell(1, 7), cell(2, 7), cell(3, 7)];
        let mut unavailable = HashSet::default();
        unavailable.insert(cell(3, 7));
        let previous = BTreeMap::from([
            (units[0], cell(1, 7)),
            (units[1], cell(2, 7)),
            (units[2], cell(3, 7)),
        ]);
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        // 兵1は上位の仕事へ移った。兵2のマスには敵が入った。
        let result = assign(
            &previous,
            &input,
            |unit| unit != units[1],
            |_, _| Some((1, 0)),
        );
        // 兵0は引き継ぐ。敵の入った(3,7)は誰にも割り当てず、空いた(2,7)は候補の誰か1体が埋める。
        assert_eq!(result[&units[0]], cell(1, 7));
        assert!(!result.values().any(|c| *c == cell(3, 7)));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn 到達できないマスには割り当てない() {
        let units = entities(1);
        let line = [cell(1, 7)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        let result = assign(&BTreeMap::new(), &input, |_| true, |_, _| None);
        assert!(result.is_empty());
    }

    #[test]
    fn 前線都市を先に並べその中を最寄り歩兵の距離順にする() {
        // y=5 が前線都市（距離2）、y=3 が後方都市（距離4）。歩兵は中央 (5,4) 付近にいる想定。
        let front = front_cities(&[
            (cell(1, 5), 2),
            (cell(5, 5), 2),
            (cell(9, 5), 2),
            (cell(5, 3), 4),
        ]);
        assert!(front.contains(&cell(1, 5)));
        assert!(front.contains(&cell(5, 5)));
        assert!(front.contains(&cell(9, 5)));
        assert!(!front.contains(&cell(5, 3)));

        let mut targets = vec![cell(5, 3), cell(9, 5), cell(1, 5), cell(5, 5)];
        let infantry = cell(5, 4);
        sort_capture_targets_front_first(&mut targets, &front, |target| {
            infantry.x.abs_diff(target.x) + infantry.y.abs_diff(target.y)
        });
        // 後方都市 (5,3) は歩兵に近くても前線都市より後ろ。前線都市の中では中央 (5,5) が先頭。
        assert_eq!(targets[0], cell(5, 5));
        assert_eq!(targets[3], cell(5, 3));
    }

    #[test]
    fn 占領中の拠点は未所有かつ捕獲ポイント減少で判定する() {
        let me = PlayerId(2);
        let enemy = PlayerId(1);
        assert!(is_capturing_in_progress(None, me, 10, 20));
        assert!(is_capturing_in_progress(Some(enemy), me, 10, 20));
        assert!(!is_capturing_in_progress(None, me, 20, 20));
        assert!(!is_capturing_in_progress(Some(me), me, 10, 20));
    }

    #[test]
    fn 占領任務中の兵は引き抜かず_前線都市の兵と割当済みの兵だけを線に向ける() {
        // 占領中（捕獲ポイント減少）は常に不可
        assert!(!can_hold_line_in_forced_mode(true, true, true, true));
        // 占領任務を持って移動中の兵は引き抜かない
        assert!(!can_hold_line_in_forced_mode(false, false, false, true));
        // 所有済み前線都市上の兵は占領任務があっても解放する
        assert!(can_hold_line_in_forced_mode(false, false, true, true));
        // すでに線へ割当済みの兵は、占領任務を再付与されても維持する
        assert!(can_hold_line_in_forced_mode(false, true, false, true));
        // 占領任務のないフリー歩兵は対象になる
        assert!(can_hold_line_in_forced_mode(false, false, false, false));
    }

    #[test]
    fn 危険な都市の守備兵は森へ転用しない() {
        let units = entities(1);
        let city = cell(8, 5);
        let line = [cell(8, 6)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        let guards = city_guards(&[(units[0], city)], |_, _| true);
        // 前回は森へ割り当てていても、都市担当にしたら森の割当から外す。
        let previous = BTreeMap::from([(units[0], cell(8, 6))]);
        let plan =
            assign_with_city_guards(&previous, &input, guards, |_| true, |_, _| Some((1, 0)));
        assert_eq!(plan.guards[&units[0]], city);
        assert!(plan.line.is_empty());
        assert_eq!(plan.unfilled, vec![cell(8, 6)]);
    }

    #[test]
    fn 安全な都市の兵は従来どおり森へ向ける() {
        let units = entities(1);
        let line = [cell(8, 6)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        let guards = city_guards(&[(units[0], cell(8, 5))], |_, _| false);
        assert!(guards.is_empty());
        let plan = assign_with_city_guards(
            &BTreeMap::new(),
            &input,
            guards,
            |_| true,
            |_, _| Some((1, 0)),
        );
        assert_eq!(plan.line[&units[0]], cell(8, 6));
        assert!(plan.unfilled.is_empty());
    }

    #[test]
    fn 都市を守ったまま届く別の兵で森を埋める() {
        let units = entities(2);
        let line = [cell(8, 6)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        let guards = city_guards(&[(units[0], cell(8, 5))], |_, _| true);
        // 守備兵の方が森に近くても、森は別の兵が埋める。
        let plan = assign_with_city_guards(
            &BTreeMap::new(),
            &input,
            guards,
            |_| true,
            |u, _| Some(if u == units[0] { (0, 1) } else { (1, 0) }),
        );
        assert_eq!(plan.guards.len(), 1);
        assert_eq!(plan.line[&units[1]], cell(8, 6));
        assert!(plan.unfilled.is_empty());
    }

    #[test]
    fn 届く兵がいない森は空白のまま未充足として返す() {
        let units = entities(2);
        let line = [cell(8, 6), cell(9, 7)];
        let unavailable = HashSet::default();
        let input = AssignInput {
            line: &line,
            unavailable: &unavailable,
            pool: &units,
        };
        let guards = city_guards(&[(units[0], cell(8, 5))], |_, _| true);
        // 兵1は (8,6) にだけ届く。(9,7) には誰も届かない。
        let plan = assign_with_city_guards(
            &BTreeMap::new(),
            &input,
            guards,
            |_| true,
            |u, c| (u == units[1] && c == cell(8, 6)).then_some((1, 0)),
        );
        assert_eq!(plan.line[&units[1]], cell(8, 6));
        assert_eq!(plan.unfilled, vec![cell(9, 7)]);
    }

    #[test]
    fn 奪われた都市と敵が上にいる自軍都市だけを奪還対象にする() {
        let me = PlayerId(2);
        let enemy = PlayerId(1);
        let lost = lost_cities(
            &[
                (cell(9, 5), Some(enemy), false),
                (cell(8, 5), Some(me), true),
                (cell(7, 5), Some(me), false),
                (cell(6, 5), None, true),
            ],
            me,
        );
        // 中立都市は通常の占領順に任せ、敵のいない自軍都市は守備の対象（奪還ではない）。
        assert_eq!(lost, HashSet::from_iter([cell(9, 5), cell(8, 5)]));
    }
}
