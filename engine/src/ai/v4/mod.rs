//! V4: 作戦駆動生産 AI。
//!
//! V1〜V3 の生産ロジック（`GamePhase` ごとの理想構成をハードコードし、
//! 構成比の差分で買うものを決める方式）とは完全に分離した独立モジュール。
//!
//! 基本方針:
//! 1. 盤面から「作戦（Operation）」＝獲る／守るべき拠点のまとまりを切り出す
//! 2. 各作戦について、観測量だけから 5 つの枠を逆算する
//!    （占領枠・撃破枠・護衛枠・輸送枠・迎撃枠）
//! 3. 空いている生産枠を、最も不足している枠から順に埋める
//!
//! 「敵を減らす」と「占領する」は別フェーズではなく同一作戦の別枠として
//! 同時に立つため、倒してから占領するのではなく並行して進む。

pub mod operation;
pub mod trace;

use operation::{
    AcquisitionMode, OperationFacts, OperationKind, OperationSlots, RESERVATION_PATIENCE_TURNS,
    SLOT_PRIORITY, SlotKind, SlotTier, acquisition_mode, derive_slots,
};
use trace::{
    ProductionDecision, ProductionOperationTrace, ProductionPlanTrace, ProductionStepTrace,
    ProductionTraceDiagnostics,
};

use crate::ai::turn_distance::TerrainConnectivity;
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Players, Terrain, UnitRegistry, UnitType};
use crate::systems::transport::can_unload_from_terrain;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// 揚陸可否キャッシュのキー：(輸送の移動タイプ, 積荷の移動タイプ, 出発地, 目標)
type DeliveryKey = (MovementType, MovementType, (usize, usize), (usize, usize));

/// 到達性まわりの計算結果を、1 回の生産判断のあいだだけ再利用するためのコンテキスト。
///
/// 揚陸可否の判定はマップ全域の走査を伴うため、施設と目標の組み合わせごとに
/// 結果を憶えておかないと候補評価のたびに同じ探索を繰り返すことになる。
#[derive(Default)]
struct ReachCtx {
    terrain: TerrainConnectivity,
    delivery: HashMap<DeliveryKey, bool>,
}

impl ReachCtx {
    /// 地形連結による到達判定（`TerrainConnectivity` への委譲）。
    fn is_reachable(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
    ) -> bool {
        self.terrain
            .is_reachable(map, registry, start, target, movement_type)
    }
}

/// 拠点をひとつの作戦にまとめる距離の閾値。
const OPERATION_CLUSTER_RADIUS: u32 = 3;

/// 同時に抱える作戦の最大数。多すぎると戦力が分散するため制限する。
const MAX_OPERATIONS: usize = 4;

/// 敵がこのターン数以内に到達できる自軍拠点は防衛作戦の対象とする。
const DEFENSE_THREAT_ETA: u32 = 2;

/// 敵の「拡張装置」（占領可能ユニットと、それを運ぶ輸送）に掛ける脅威の倍率。
/// これらはコスト以上に盤面の収入を動かすため、素のコスト価値で数えると
/// 撃破枠が立たず、局地戦で勝ちながら territory を明け渡すことになる。
const EXPANSION_THREAT_WEIGHT: u32 = 2;

/// 盤面から取り出したユニット 1 体分の情報。
#[derive(Debug, Clone)]
struct UnitSnapshot {
    pos: GridPosition,
    stats: UnitStats,
    hp: u32,
    free_cargo: u32,
}

impl UnitSnapshot {
    /// HP を加味した戦力価値。
    fn value(&self) -> u32 {
        self.stats.cost.saturating_mul(self.hp) / 100
    }
}

/// 1体の敵に対する未対処戦力。
///
/// `remaining_value` はHP補正済みの実戦力、`priority_weight` は倒す順序にだけ使う。
/// 占領・輸送能力の戦略的重要度を実戦力へ掛けると「重要だから2体必要」という
/// 誤った需要になるため、両者を別の次元として保持する。
#[derive(Debug, Clone)]
struct ThreatTarget {
    stats: UnitStats,
    position: GridPosition,
    remaining_value: f32,
    priority_weight: f32,
}

impl ThreatTarget {
    fn from_snapshot(unit: &UnitSnapshot, expansion_race_live: bool) -> Self {
        let priority_weight =
            if expansion_race_live && (unit.stats.can_capture || unit.stats.max_cargo > 0) {
                EXPANSION_THREAT_WEIGHT as f32
            } else {
                1.0
            };
        Self {
            stats: unit.stats.clone(),
            position: unit.pos,
            remaining_value: unit.value() as f32,
            priority_weight,
        }
    }
}

/// 1 つの作戦。対象拠点のまとまりと、そこから導出された枠を保持する。
#[derive(Debug)]
struct Operation {
    kind: OperationKind,
    /// 作戦の代表地点（距離計算の基準）
    anchor: GridPosition,
    facts: OperationFacts,
    slots: OperationSlots,
    /// この生産計画の中で既に購入した分
    filled: OperationSlots,
    /// 自軍が生産しうるどの移動タイプでも到達できない位置にいる敵（＝迎え撃つしかない敵）
    unreachable_threats: Vec<ThreatTarget>,
    /// 自軍が生産しうるいずれかの移動タイプで到達できる位置にいる敵（＝殴りに行ける敵）
    reachable_threats: Vec<ThreatTarget>,
}

/// 1手番の全施設について一度だけ作ったV4生産計画。
/// 1命令ごとの再計画で残存脅威台帳を初期化しないため、同じ計画を順に消費する。
#[derive(Resource, Debug, Default)]
struct V4ProductionTurnPlan {
    player_id: Option<PlayerId>,
    turn: u32,
    commands: VecDeque<ProduceUnitCommand>,
}

/// 生産候補 1 件。
#[derive(Debug, Clone, Copy)]
struct SlotCandidate {
    unit_type: UnitType,
    cost: u32,
    facility: GridPosition,
    /// 枠への適合度。大きいほど良い。
    fitness: f32,
}

/// V4 の生産意思決定エントリポイント。
///
/// `decide_production` から `AiVersion::uses_operation_driven_production()` が
/// true のときだけ委譲される。V1/V2/V3 の経路には一切影響しない。
pub fn decide_production_v4(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let mut turn_plan = world
        .remove_resource::<V4ProductionTurnPlan>()
        .unwrap_or_default();
    if turn_plan.player_id == Some(player_id) && turn_plan.turn == turn {
        let next = turn_plan.commands.pop_front();
        world.insert_resource(turn_plan);
        return next.into_iter().collect();
    }

    let Some(scan) = BoardScan::collect(world, player_id) else {
        world.insert_resource(turn_plan);
        return Vec::new();
    };
    let (commands, plan_trace) = plan_production(&scan, player_id);

    // 生産判断の内訳を診断リソースへ残す（判定は行わず記録のみ）。
    if let Some(mut diagnostics) = world.get_resource_mut::<ProductionTraceDiagnostics>() {
        diagnostics.record(turn, plan_trace);
    } else {
        let mut diagnostics = ProductionTraceDiagnostics::default();
        diagnostics.record(turn, plan_trace);
        world.insert_resource(diagnostics);
    }

    turn_plan.player_id = Some(player_id);
    turn_plan.turn = turn;
    turn_plan.commands = VecDeque::from(commands);
    let next = turn_plan.commands.pop_front();
    world.insert_resource(turn_plan);
    next.into_iter().collect()
}

/// 盤面から生産判断に必要な観測量をすべて取り出したもの。
struct BoardScan {
    map: Map,
    master_data: MasterDataRegistry,
    damage_chart: DamageChart,
    funds: u32,
    /// 生産可能な施設（未占有・生産範囲内・クールダウン対象外）
    free_facilities: Vec<(GridPosition, Terrain)>,
    available_types: Vec<(UnitType, UnitStats)>,
    my_units: Vec<UnitSnapshot>,
    enemy_units: Vec<UnitSnapshot>,
    my_properties: Vec<GridPosition>,
    /// 自軍が保有していない拠点（中立・敵）
    open_properties: Vec<GridPosition>,
    enemy_income: u32,
    enemy_production_slots: u32,
    my_income: u32,
}

impl BoardScan {
    fn collect(world: &mut World, player_id: PlayerId) -> Option<Self> {
        let map = world.get_resource::<Map>()?.clone();
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = world.get_resource::<DamageChart>()?.clone();
        let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
        let funds = world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)?;

        // 同一ターン内で生産に失敗した施設を除外するためのクールダウン
        let cooldown: HashSet<(usize, usize)> = world
            .get_resource::<crate::ai::engine::AiProductionCooldown>()
            .map(|c| c.0.clone())
            .unwrap_or_default();

        // --- ユニットの走査 ---
        let mut occupied = HashSet::new();
        let mut my_units = Vec::new();
        let mut enemy_units = Vec::new();
        {
            let mut q = world.query::<(
                &GridPosition,
                &Faction,
                &UnitStats,
                Option<&Health>,
                Option<&CargoCapacity>,
                Option<&Transporting>,
            )>();
            for (pos, faction, stats, health, cargo, transporting) in q.iter(world) {
                // 輸送中のユニットは盤面を占有しない
                if transporting.is_some() {
                    continue;
                }
                occupied.insert(*pos);
                let snapshot = UnitSnapshot {
                    pos: *pos,
                    hp: health.map_or(100, |h| h.current),
                    free_cargo: cargo.map_or(stats.max_cargo, |c| {
                        stats.max_cargo.saturating_sub(c.loaded.len() as u32)
                    }),
                    stats: stats.clone(),
                };
                if faction.0 == player_id {
                    my_units.push(snapshot);
                } else {
                    enemy_units.push(snapshot);
                }
            }
        }

        // --- 拠点の走査 ---
        let mut capital_pos = None;
        let mut my_properties = Vec::new();
        let mut open_properties = Vec::new();
        let mut facilities = Vec::new();
        let mut enemy_income = 0u32;
        let mut enemy_production_slots = 0u32;
        let mut my_income = 0u32;
        {
            let mut q = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in q.iter(world) {
                if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                }
            }
            for (pos, prop) in q.iter(world) {
                let income = master_data.landscape_income(prop.terrain.as_str());
                let is_facility = master_data.is_production_facility(prop.terrain.as_str());
                match prop.owner_id {
                    Some(owner) if owner == player_id => {
                        my_income = my_income.saturating_add(income);
                        my_properties.push(*pos);
                        // 首都・都市を含む生産施設のうち、生産範囲内かつ空いているもの
                        if is_facility
                            && !occupied.contains(pos)
                            && !cooldown.contains(&(pos.x, pos.y))
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            )
                        {
                            facilities.push((*pos, prop.terrain));
                        }
                    }
                    Some(_) => {
                        enemy_income = enemy_income.saturating_add(income);
                        if is_facility {
                            enemy_production_slots += 1;
                        }
                        open_properties.push(*pos);
                    }
                    None => open_properties.push(*pos),
                }
            }
        }

        if facilities.is_empty() {
            return None;
        }

        let available_types: Vec<(UnitType, UnitStats)> = unit_registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect();

        Some(BoardScan {
            map,
            master_data,
            damage_chart,
            funds,
            free_facilities: facilities,
            available_types,
            my_units,
            enemy_units,
            my_properties,
            open_properties,
            enemy_income,
            enemy_production_slots,
            my_income,
        })
    }

    /// 指定した拠点で `unit_type` を生産できるか。
    fn can_produce(&self, terrain: Terrain, unit_type: UnitType) -> bool {
        self.master_data
            .can_produce_unit(terrain.as_str(), unit_type)
    }

    /// 生産可能なユニットのうち、占領可能で最も安いものを「基準占領ユニット」とする。
    /// 展開リードタイムや到達可能性はこのユニットの足で測る。
    fn reference_capture_unit(&self) -> Option<&UnitStats> {
        self.available_types
            .iter()
            .filter(|(unit_type, stats)| {
                stats.can_capture
                    && self
                        .free_facilities
                        .iter()
                        .any(|(_, terrain)| self.can_produce(*terrain, *unit_type))
            })
            .min_by_key(|(_, stats)| stats.cost)
            .map(|(_, stats)| stats)
    }
}

/// 距離と移動力から到達ターン数を見積もる。
fn eta_turns(map: &Map, from: &GridPosition, to: &GridPosition, movement: u32) -> u32 {
    let distance = map.distance(from.x, from.y, to.x, to.y);
    distance.div_ceil(movement.max(1))
}

/// 位置の集合を、距離 `radius` 以内で連なるまとまりに分割する。
///
/// トポロジ依存の隣接判定は `Map::distance` に委譲しているため、
/// スクエア／ヘックスのどちらでも同じコードで動作する。
fn cluster_positions(positions: &[GridPosition], map: &Map, radius: u32) -> Vec<Vec<GridPosition>> {
    // 決定性のために座標順へ整列してから処理する
    let mut sorted = positions.to_vec();
    sorted.sort_by_key(|p| (p.x, p.y));

    let mut clusters: Vec<Vec<GridPosition>> = Vec::new();
    for pos in sorted {
        // 既存クラスタのいずれかの要素と radius 以内なら合流する
        let hit = clusters.iter().position(|cluster| {
            cluster
                .iter()
                .any(|member| map.distance(member.x, member.y, pos.x, pos.y) <= radius)
        });
        match hit {
            Some(index) => clusters[index].push(pos),
            None => clusters.push(vec![pos]),
        }
    }

    // 合流により隣接するようになったクラスタ同士を併合する
    let mut merged: Vec<Vec<GridPosition>> = Vec::new();
    for cluster in clusters {
        let hit = merged.iter().position(|existing| {
            existing.iter().any(|a| {
                cluster
                    .iter()
                    .any(|b| map.distance(a.x, a.y, b.x, b.y) <= radius)
            })
        });
        match hit {
            Some(index) => merged[index].extend(cluster),
            None => merged.push(cluster),
        }
    }
    merged
}

/// 盤面から作戦の一覧を組み立てる。
fn build_operations(scan: &BoardScan, ctx: &mut ReachCtx) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // --- 防衛作戦: 敵が DEFENSE_THREAT_ETA 以内に「実際に到達しうる」自軍拠点 ---
    // 直線距離だけで脅威と見なすと、海を渡れない敵地上軍まで脅威に数えてしまい、
    // 盤面中の拠点が防衛作戦に化けて占領作戦を truncate で押し出してしまう。
    // 脅威かどうかも到達可能性で判定する。
    let mut threatened: Vec<GridPosition> = Vec::new();
    for pos in &scan.my_properties {
        let is_threatened = scan.enemy_units.iter().any(|enemy| {
            eta_turns(&scan.map, &enemy.pos, pos, enemy.stats.max_movement) <= DEFENSE_THREAT_ETA
        });
        if !is_threatened {
            continue;
        }
        // ETA を満たす敵のうち、地形的にもその拠点へ到達できる敵が一体でもいるか
        let reachable = scan.enemy_units.iter().any(|enemy| {
            eta_turns(&scan.map, &enemy.pos, pos, enemy.stats.max_movement) <= DEFENSE_THREAT_ETA
                && ctx.is_reachable(
                    &scan.map,
                    &scan.master_data,
                    (enemy.pos.x, enemy.pos.y),
                    (pos.x, pos.y),
                    enemy.stats.movement_type,
                )
        });
        if reachable {
            threatened.push(*pos);
        }
    }

    let mut raw: Vec<(OperationKind, Vec<GridPosition>)> =
        cluster_positions(&threatened, &scan.map, OPERATION_CLUSTER_RADIUS)
            .into_iter()
            .map(|cluster| (OperationKind::Defense, cluster))
            .collect();

    // --- 占領作戦: 自軍が保有していない拠点のまとまり ---
    raw.extend(
        cluster_positions(&scan.open_properties, &scan.map, OPERATION_CLUSTER_RADIUS)
            .into_iter()
            .map(|cluster| (OperationKind::Capture, cluster)),
    );

    // 生産施設から近い作戦を優先して MAX_OPERATIONS 件に絞る
    let mut scored: Vec<(u32, OperationKind, Vec<GridPosition>)> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let anchor = anchor_of(&cluster, scan);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            (lead, kind, cluster)
        })
        .collect();
    scored.sort_by_key(|(lead, kind, cluster)| {
        (
            kind.priority_rank(),
            *lead,
            // 同条件なら拠点数の多い（面が広い）作戦を優先
            usize::MAX - cluster.len(),
        )
    });
    // 防衛作戦は priority_rank が最上位なので、素直に truncate すると防衛だけで枠が埋まり、
    // 占領作戦が 1 つも残らずに拡張が完全停止する（＝ジリ貧）ことがある。
    // 占領目標が残っている限り、最良の占領作戦を 1 枠だけ確保する。
    let rescued_capture = if scored[..scored.len().min(MAX_OPERATIONS)]
        .iter()
        .any(|(_, kind, _)| *kind == OperationKind::Capture)
    {
        None
    } else {
        scored
            .iter()
            .position(|(_, kind, _)| *kind == OperationKind::Capture)
            .map(|index| scored.remove(index))
    };
    scored.truncate(MAX_OPERATIONS);
    if let Some(capture) = rescued_capture {
        // 最も優先度の低い枠を明け渡して占領作戦を差し込む
        if scored.len() >= MAX_OPERATIONS {
            scored.pop();
        }
        scored.push(capture);
    }

    let operation_count = scored.len().max(1) as f32;
    let anchors: Vec<GridPosition> = scored
        .iter()
        .map(|(_, _, cluster)| anchor_of(cluster, scan))
        .collect();

    scored
        .into_iter()
        .enumerate()
        .map(|(index, (lead, kind, cluster))| {
            let anchor = anchors[index];
            build_operation(
                scan,
                ctx,
                &reference,
                kind,
                anchor,
                &anchors,
                &cluster,
                lead,
                1.0 / operation_count,
            )
        })
        .collect()
}

/// クラスタの代表地点。自軍生産施設に最も近い拠点を選ぶ。
fn anchor_of(cluster: &[GridPosition], scan: &BoardScan) -> GridPosition {
    cluster
        .iter()
        .copied()
        .min_by_key(|pos| {
            scan.free_facilities
                .iter()
                .map(|(f, _)| scan.map.distance(f.x, f.y, pos.x, pos.y))
                .min()
                .unwrap_or(u32::MAX)
        })
        .unwrap_or(cluster[0])
}

/// 生産施設から代表地点までの展開リードタイム（最短）。
fn facility_lead_time(scan: &BoardScan, anchor: &GridPosition, movement: u32) -> u32 {
    scan.free_facilities
        .iter()
        .map(|(f, _)| eta_turns(&scan.map, f, anchor, movement))
        .min()
        .unwrap_or(u32::MAX)
}

/// 敵を最寄りの作戦へ一意に帰属させる。同距離なら anchor の並び順で決める。
fn nearest_anchor_index(
    map: &Map,
    pos: &GridPosition,
    movement: u32,
    anchors: &[GridPosition],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .min_by_key(|(index, anchor)| (eta_turns(map, pos, anchor, movement), *index))
        .map(|(index, _)| index)
}

/// 1 つの作戦について観測量を集め、枠を導出する。
#[allow(clippy::too_many_arguments)]
fn build_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    reference: &UnitStats,
    kind: OperationKind,
    anchor: GridPosition,
    anchors: &[GridPosition],
    cluster: &[GridPosition],
    deploy_lead_time: u32,
    frontline_share: f32,
) -> Operation {
    // この作戦を「最寄りの作戦」とするユニットだけを、この作戦の担当として数える。
    // これにより 1 体のユニットが複数作戦に二重計上されない。
    let anchor_index = anchors.iter().position(|candidate| *candidate == anchor);
    let is_nearest = |pos: &GridPosition, movement: u32| -> bool {
        anchor_index.is_some()
            && nearest_anchor_index(&scan.map, pos, movement, anchors) == anchor_index
    };

    // 基準占領ユニットが自力で到達できるかどうかで輸送要否が決まる。
    // 「島だから輸送が要る」ではなく「地形的に繋がっていないから要る」と判定する。
    let requires_transport = !scan.free_facilities.iter().any(|(f, _)| {
        ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (f.x, f.y),
            (anchor.x, anchor.y),
            reference.movement_type,
        )
    });

    // 敵戦力の仕分けは「互いに届くか」という 2 つの問いだけで決まる。
    //   1. 自軍が生産しうる何らかの移動タイプでその敵へ届くか → 届くなら撃破枠で殴りに行ける
    //   2. その敵が代表地点へ届くか                          → 届くなら放置できない脅威
    // 「こちらから届かない」かつ「敵も来られない」敵だけが交戦不成立として除外される。
    //
    // (1) を基準占領ユニット（歩兵）の足で判定してはならない。
    // 海の向こうで拠点を取り続ける敵の占領部隊は、歩兵の足では届かないというだけで
    // 脅威の集計から丸ごと消え、撃破枠が立たず、対抗候補の採点対象にもならなくなる。
    // 実際にはヘリ・艦船・航空機で届くのだから、生産しうる移動タイプ全体で問う。
    // 「制空で応じるか対空で応じるか」が思想ではなく到達可能性の問題であるのと同じで、
    // 「敵の占領部隊を潰しに行けるか」もまた到達可能性の問題でしかない。
    let producible_movement_types: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };

    // 未取得の拠点が残っている限り、占領レースは進行中である。
    // その間、敵の拡張装置は素のコスト以上の脅威として数える。
    let expansion_race_live = !scan.open_properties.is_empty();

    let mut reachable_threats = Vec::new();
    let mut unreachable_threats = Vec::new();
    let mut enemy_combat_value = 0u32;
    let mut unreachable_threat_value = 0u32;
    let mut enemy_contact_eta = u32::MAX;
    let mut enemy_cost_total = 0u32;
    for enemy in &scan.enemy_units {
        if !is_nearest(&enemy.pos, enemy.stats.max_movement) {
            continue;
        }
        let i_can_reach = producible_movement_types.iter().any(|movement_type| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (anchor.x, anchor.y),
                (enemy.pos.x, enemy.pos.y),
                *movement_type,
            )
        });
        let it_can_reach_me = ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (enemy.pos.x, enemy.pos.y),
            (anchor.x, anchor.y),
            enemy.stats.movement_type,
        );
        if !i_can_reach && !it_can_reach_me {
            // 交戦が成立しない敵。増援見積もりの母数からも外す。
            continue;
        }
        if it_can_reach_me {
            enemy_contact_eta = enemy_contact_eta.min(eta_turns(
                &scan.map,
                &enemy.pos,
                &anchor,
                enemy.stats.max_movement,
            ));
        }
        enemy_cost_total = enemy_cost_total.saturating_add(enemy.stats.cost);
        if i_can_reach {
            let threat = threat_value(enemy, expansion_race_live);
            enemy_combat_value = enemy_combat_value.saturating_add(threat);
            reachable_threats.push(ThreatTarget::from_snapshot(enemy, expansion_race_live));
        } else {
            let threat = threat_value(enemy, expansion_race_live);
            unreachable_threat_value = unreachable_threat_value.saturating_add(threat);
            unreachable_threats.push(ThreatTarget::from_snapshot(enemy, expansion_race_live));
        }
    }
    let committed_enemy_count = (reachable_threats.len() + unreachable_threats.len()) as u32;

    // --- 自軍戦力の仕分け ---
    // 敵の仕分けが済んでから数える。
    //
    // 台帳（既存戦力の計上）は、必ず `slot_fitness` の採用条件と同じ判定を使う。
    // 両者がずれると「その枠を埋められるのに、その枠の充足としては数えられない」
    // ユニットが生まれ、枠の要求が永久に減らずに同じユニットを買い続けるラチェットになる。
    // そのため排他に振り分けず、埋められる枠すべてに計上する。
    // （1 体の対空ユニットが航空脅威の抑止と地上の頭数を兼ねるのは実態としても正しい）
    //
    // ここで「最寄りの作戦か」で仕分けてはならない。自軍ユニットは自陣の生産施設で
    // 生まれるので、位置で排他に割り振ると母港に近い作戦が全部を吸い、渡洋作戦側の
    // 台帳は永久に 0 のままになる。要求が一切減らないので同じユニットを毎ターン
    // 買い続けるラチェットになる（実測で歩兵 56 体、揚陸艇 7 隻、対空 20 両）。
    // 上限（`MAX_CAPTURE_SLOTS` 等）はあくまで 1 波の規模であって、
    // 「既に持っている分」を差し引く役割は担っていない。差し引きはこの台帳の仕事。
    let mut friendly_capture_units_committed = 0u32;
    let mut friendly_combat_value_committed = 0u32;
    let mut friendly_intercept_value_committed = 0u32;
    let mut available_free_cargo_slots = 0u32;
    for unit in &scan.my_units {
        if unit.stats.can_capture {
            // 占領枠の採用条件と同じ関数で数える（自力到達 or 輸送の当てがある）
            if can_join_operation(
                scan,
                ctx,
                &anchor,
                requires_transport,
                &unit.pos,
                &unit.stats,
            ) {
                friendly_capture_units_committed += 1;
            }
        } else if unit.stats.max_cargo == 0 {
            // 迎撃枠の条件：到達できない脅威へ有効打を持ち、かつ自力で現地へ行ける
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (unit.pos.x, unit.pos.y),
                (anchor.x, anchor.y),
                unit.stats.movement_type,
            );
            if self_deployable
                && threats_have_counter(
                    &unit.stats,
                    &unreachable_threats,
                    &(0..unreachable_threats.len()).collect::<Vec<_>>(),
                    &scan.damage_chart,
                )
            {
                friendly_intercept_value_committed =
                    friendly_intercept_value_committed.saturating_add(unit.value());
                let indices: Vec<usize> = (0..unreachable_threats.len()).collect();
                apply_coverage(
                    &unit.stats,
                    unit.value() as f32,
                    &mut unreachable_threats,
                    &indices,
                    &scan.damage_chart,
                );
            }
            // 撃破枠の条件：現地へ行けて、自分が実際に届く敵に対して有効打を持つ。
            // 敵が観測できない段階では誰でも採用されうるので、台帳側も同様に全員を数える。
            if !can_join_operation(
                scan,
                ctx,
                &anchor,
                requires_transport,
                &unit.pos,
                &unit.stats,
            ) {
                continue;
            }
            let origin = if self_deployable { unit.pos } else { anchor };
            let engageable =
                reachable_threat_indices(scan, ctx, &reachable_threats, origin, &unit.stats);
            let combat_eligible = reachable_threats.is_empty()
                || threats_have_counter(
                    &unit.stats,
                    &reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
            if combat_eligible {
                friendly_combat_value_committed =
                    friendly_combat_value_committed.saturating_add(unit.value());
                apply_coverage(
                    &unit.stats,
                    unit.value() as f32,
                    &mut reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
            }
        }
    }

    // 輸送台帳も同じ原則で数える。輸送枠の採用条件は「その積荷をこの作戦地点へ
    // 届けられるか」なので、台帳も同じく `can_deliver_cargo` で数える。
    let cargo_movements: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .filter(|(_, stats)| stats.can_capture)
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };
    for unit in &scan.my_units {
        if unit.free_cargo == 0 {
            continue;
        }
        let deliverable = cargo_movements.iter().any(|cargo_movement| {
            can_deliver_cargo(
                &scan.map,
                &scan.master_data,
                ctx,
                &unit.pos,
                &anchor,
                unit.stats.movement_type,
                *cargo_movement,
            )
        });
        if deliverable {
            available_free_cargo_slots = available_free_cargo_slots.saturating_add(unit.free_cargo);
        }
    }

    let enemy_average_unit_cost = enemy_cost_total
        .checked_div(committed_enemy_count)
        .unwrap_or_else(|| {
            // 敵ユニットが観測できないうちは、自軍が生産しうるユニットの平均コストで代用する
            let total: u32 = scan
                .available_types
                .iter()
                .map(|(_, stats)| stats.cost)
                .sum();
            total / (scan.available_types.len().max(1) as u32)
        });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        friendly_combat_value_committed,
        friendly_intercept_value_committed,
        enemy_combat_value,
        enemy_income: scan.enemy_income,
        enemy_production_slots: scan.enemy_production_slots,
        enemy_average_unit_cost,
        frontline_share,
        deploy_lead_time,
        enemy_contact_eta: if enemy_contact_eta == u32::MAX {
            u32::MAX
        } else {
            enemy_contact_eta
        },
        requires_transport,
        transport_round_trip_turns,
        available_free_cargo_slots,
        unreachable_threat_value,
        // 展開リードタイムの間に投入しうる資金。今ある資金だけで測ると、
        // 収入が大きい局面でも要求が現在資金に張り付いてしまう。
        friendly_spendable_funds: scan
            .funds
            .saturating_add(scan.my_income.saturating_mul(deploy_lead_time)),
    };

    Operation {
        kind,
        anchor,
        slots: derive_slots(&facts),
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
    }
}

/// 作戦一覧と資金から、この生産フェーズで発行する生産命令を組み立てる。
fn plan_production(
    scan: &BoardScan,
    player_id: PlayerId,
) -> (Vec<ProduceUnitCommand>, ProductionPlanTrace) {
    let mut ctx = ReachCtx::default();
    let mut operations = build_operations(scan, &mut ctx);
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());

    if operations.is_empty() {
        plan_trace.fallback = true;
        return (fallback_production(scan, player_id), plan_trace);
    }

    // 同格の作戦は「敵の到達が早い順」に処理する
    operations.sort_by_key(|op| (op.kind.priority_rank(), op.facts.enemy_contact_eta));

    plan_trace.operations = operations
        .iter()
        .map(|op| ProductionOperationTrace {
            kind: op.kind,
            anchor: op.anchor,
            slots: op.slots,
            requires_transport: op.facts.requires_transport,
            enemy_combat_value: op.facts.enemy_combat_value,
            friendly_combat_value_committed: op.facts.friendly_combat_value_committed,
            deploy_lead_time: op.facts.deploy_lead_time,
        })
        .collect();

    let mut used_facilities: HashSet<GridPosition> = HashSet::new();
    let mut remaining_funds = scan.funds;
    let mut commands = Vec::new();

    loop {
        let free_slots = scan
            .free_facilities
            .iter()
            .filter(|(pos, _)| !used_facilities.contains(pos))
            .count();
        if free_slots == 0 {
            break;
        }
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = remaining_funds / free_slots as u32;

        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(&operations) else {
            break;
        };

        // トレース用に、選定前の未充足率と作戦の識別情報を控えておく。
        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let deficit_before = operations[op_index]
            .slots
            .deficit_ratio(slot_kind, &operations[op_index].filled);
        let remaining_funds_before = remaining_funds;

        let candidate = select_candidate(
            scan,
            &mut ctx,
            &operations[op_index],
            slot_kind,
            &used_facilities,
            remaining_funds,
            per_slot_budget,
        );

        let Some(candidate) = candidate else {
            // この枠を満たせる候補が無い場合は、枠の要求を落として次を探す
            clear_slot(&mut operations[op_index], slot_kind);
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: ProductionDecision::SlotCleared,
            });
            continue;
        };

        // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
        // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
        if should_defer_purchase(
            scan,
            &mut ctx,
            &operations[op_index],
            slot_kind,
            remaining_funds,
            candidate.cost,
        ) {
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: ProductionDecision::Deferred {
                    unit_type: candidate.unit_type,
                    cost: candidate.cost,
                },
            });
            break;
        }

        remaining_funds = remaining_funds.saturating_sub(candidate.cost);
        used_facilities.insert(candidate.facility);
        record_fill(
            scan,
            &mut ctx,
            &mut operations[op_index],
            slot_kind,
            &candidate,
        );
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind,
            deficit_before,
            // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
            deficit_after: operations[op_index]
                .slots
                .deficit_ratio(slot_kind, &operations[op_index].filled),
            remaining_funds_before,
            decision: ProductionDecision::Produced {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        commands.push(ProduceUnitCommand {
            player_id,
            target_x: candidate.facility.x,
            target_y: candidate.facility.y,
            unit_type: candidate.unit_type,
        });
    }

    plan_trace.leftover_funds = remaining_funds;
    (commands, plan_trace)
}

/// 次に埋めるべき枠を返す。
///
/// 2 段階で選ぶ。要求が有限の枠（前提条件）を全作戦ぶん先に満たし、
/// そのうえで残額を要求が青天井の撃破枠へ注ぎ込む。
/// 有限要求と青天井要求を同じ土俵で比べてはならない（`SlotTier` 参照）。
fn most_starved_slot(operations: &[Operation]) -> Option<(usize, SlotKind)> {
    most_starved_in_tier(operations, SlotTier::Prerequisite)
        .or_else(|| most_starved_in_tier(operations, SlotTier::Residual))
}

/// 指定した段階の中で最も飢えた枠を返す。
fn most_starved_in_tier(operations: &[Operation], tier: SlotTier) -> Option<(usize, SlotKind)> {
    let mut best: Option<(usize, SlotKind, (u32, usize, f32))> = None;
    for (index, op) in operations.iter().enumerate() {
        for (priority, kind) in SLOT_PRIORITY.iter().enumerate() {
            let deficit = op.slots.tier_deficit(*kind, &op.filled, tier);
            if deficit <= 0.0 {
                continue;
            }
            let key = match tier {
                // 前提条件は「どの作戦を先に成立させるか」で並べる。
                // 作戦の優先度 → 枠の固定優先順位（SLOT_PRIORITY は作戦遂行の
                // 前提から順に並んでいる）→ 未充足率。
                SlotTier::Prerequisite => (op.kind.priority_rank(), priority, -deficit),
                // 余剰は作戦の別なく、未充足率だけで配る。
                // ここで作戦優先度を先に見てはならない。撃破枠の要求は青天井なので、
                // 最優先の作戦（＝自陣の防衛）が全額を吸い、渡洋作戦には 1 円も
                // 回らなくなる＝自陣に引きこもる。撃破要求は既に前線ごとの分担比
                // (`frontline_share`) で割ってあるので、未充足率で選べば
                // 資金は自然と各前線の分担比どおりに配分される。
                SlotTier::Residual => (0, 0, -deficit),
            };
            if best.is_none_or(|(_, _, best_key)| key < best_key) {
                best = Some((index, *kind, key));
            }
        }
    }
    best.map(|(index, kind, _)| (index, kind))
}

/// 満たせないと判明した枠の要求を消す（無限ループ防止）。
fn clear_slot(op: &mut Operation, kind: SlotKind) {
    match kind {
        SlotKind::Intercept => op.slots.intercept_budget = 0,
        SlotKind::Transport => op.slots.transport_slots = 0,
        SlotKind::Capture => op.slots.capture_units = 0,
        SlotKind::Combat => {
            op.slots.escort_units = 0;
            op.slots.destroy_budget = 0;
        }
    }
}

/// 購入した 1 体分を充足量へ反映する。
fn record_fill(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &mut Operation,
    kind: SlotKind,
    candidate: &SlotCandidate,
) {
    let cargo = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats.max_cargo)
        .unwrap_or(0);
    match kind {
        SlotKind::Intercept => {
            op.filled.intercept_budget = op.filled.intercept_budget.saturating_add(candidate.cost);
            let indices: Vec<usize> = (0..op.unreachable_threats.len()).collect();
            apply_coverage(
                candidate_stats(scan, candidate),
                candidate.cost as f32,
                &mut op.unreachable_threats,
                &indices,
                &scan.damage_chart,
            );
        }
        SlotKind::Transport => {
            op.filled.transport_slots = op.filled.transport_slots.saturating_add(cargo.max(1))
        }
        SlotKind::Capture => op.filled.capture_units += 1,
        SlotKind::Combat => {
            // 戦闘ユニットの購入は護衛枠（体数）と撃破枠（資金）を同時に満たす
            op.filled.escort_units += 1;
            op.filled.destroy_budget = op.filled.destroy_budget.saturating_add(candidate.cost);
            let stats = candidate_stats(scan, candidate);
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (candidate.facility.x, candidate.facility.y),
                (op.anchor.x, op.anchor.y),
                stats.movement_type,
            );
            let origin = if self_deployable {
                candidate.facility
            } else {
                op.anchor
            };
            let indices = reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats);
            apply_coverage(
                stats,
                candidate.cost as f32,
                &mut op.reachable_threats,
                &indices,
                &scan.damage_chart,
            );
        }
    }
}

/// 候補の能力値を生産可能一覧から復元する。候補は同じ一覧から生成されるため必ず存在する。
fn candidate_stats<'a>(scan: &'a BoardScan, candidate: &SlotCandidate) -> &'a UnitStats {
    scan.available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats)
        .expect("生産候補は生産可能ユニット一覧に存在する")
}

/// 指定地点から実際に交戦できる脅威の添字を返す。
fn reachable_threat_indices(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    threats: &[ThreatTarget],
    origin: GridPosition,
    stats: &UnitStats,
) -> Vec<usize> {
    threats
        .iter()
        .enumerate()
        .filter(|(_, threat)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (origin.x, origin.y),
                (threat.position.x, threat.position.y),
                stats.movement_type,
            )
        })
        .map(|(index, _)| index)
        .collect()
}

fn threats_have_counter(
    unit: &UnitStats,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> bool {
    eligible_indices
        .iter()
        .any(|index| coverage_efficiency(unit, &threats[*index].stats, chart) > 0.0)
}

/// 価値交換効率を、脅威被覆に使う 0.0..=1.0 の効率へ変換する。
fn coverage_efficiency(unit: &UnitStats, enemy: &UnitStats, chart: &DamageChart) -> f32 {
    if enemy.cost == 0 {
        return 0.0;
    }
    (pair_value(unit, enemy, chart).max(0.0) / enemy.cost as f32).clamp(0.0, 1.0)
}

/// 1体の対抗ユニットを、重要度×相性が高い未対処脅威から順に割り当てる。
/// 戻り値は戦略的重要度を掛けた被覆増分で、候補採点と実台帳更新が同じ関数を通る。
fn apply_coverage(
    unit: &UnitStats,
    capacity: f32,
    threats: &mut [ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut remaining_capacity = capacity.max(0.0);
    let mut weighted_coverage = 0.0;
    while remaining_capacity > 0.0 {
        let Some((index, efficiency)) = eligible_indices
            .iter()
            .map(|index| {
                let threat = &threats[*index];
                (*index, coverage_efficiency(unit, &threat.stats, chart))
            })
            .filter(|(index, efficiency)| {
                threats[*index].remaining_value > 0.0 && *efficiency > 0.0
            })
            .max_by(
                |(left_index, left_efficiency), (right_index, right_efficiency)| {
                    let left = *left_efficiency * threats[*left_index].priority_weight;
                    let right = *right_efficiency * threats[*right_index].priority_weight;
                    left.total_cmp(&right)
                        .then_with(|| right_index.cmp(left_index))
                },
            )
        else {
            break;
        };
        let effective_capacity = remaining_capacity * efficiency;
        let covered = threats[index].remaining_value.min(effective_capacity);
        if covered <= 0.0 {
            break;
        }
        threats[index].remaining_value -= covered;
        remaining_capacity -= covered / efficiency;
        weighted_coverage += covered * threats[index].priority_weight;
    }
    weighted_coverage
}

/// 残存脅威へ新たに与えられる被覆量。候補の比較は平均相性ではなくこの増分で行う。
fn marginal_coverage(
    unit: &UnitStats,
    capacity: f32,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut projected = threats.to_vec();
    apply_coverage(unit, capacity, &mut projected, eligible_indices, chart)
}

/// 指定の枠を満たす最良の候補を選ぶ。
fn select_candidate(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    used_facilities: &HashSet<GridPosition>,
    remaining_funds: u32,
    per_slot_budget: u32,
) -> Option<SlotCandidate> {
    let mut best: Option<SlotCandidate> = None;
    let mut best_over_budget: Option<SlotCandidate> = None;

    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) {
                continue;
            }
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
                continue;
            };
            if stats.cost == 0 || stats.cost > remaining_funds {
                continue;
            }
            // 枠の要求単位は種別ごとに違う（`OperationSlots::requirement` 参照）ので、
            // 1 購入あたりの機会費用も種別で変える。
            // - 占領枠／輸送枠は要求が「体数」「スロット数」。1 購入で満たせる要求は
            //   価格に関わらず 1 でしかないため、高い候補を買うほど同じ要求を満たす
            //   総額が膨らむ。ここは支払額そのものが機会費用になる。
            // - 撃破枠／迎撃枠は要求が「資金」。1 ターンに使える生産枠数は固定なので、
            //   投入戦力を増やす唯一の手段は 1 枠あたりの戦力を上げることであり、
            //   安く済ませても余剰はその枠では使えず割引にならない。よって分母は
            //   cost と per_slot_budget の大きい方を取り、枠あたり戦力で比較する。
            let count_denominated = matches!(kind, SlotKind::Capture | SlotKind::Transport);
            let opportunity_cost = if count_denominated {
                stats.cost
            } else {
                stats.cost.max(per_slot_budget)
            }
            .max(1);
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                fitness: fitness * 1000.0 / opportunity_cost as f32,
            };
            // 予算内／予算超過の階層分けも体数系の枠にだけ残す。資金系の枠でこれを
            // やると、どれほど弱くても予算内の候補が常に強い候補に勝ってしまい、
            // 資金が潤沢でも安いユニットしか買わなくなる（＝戦力の逐次投入）。
            let slot = if count_denominated && stats.cost > per_slot_budget.max(1) {
                &mut best_over_budget
            } else {
                &mut best
            };
            let better = slot.is_none_or(|current| {
                if count_denominated {
                    // 同性能なら安い方が多く揃う
                    (candidate.fitness, current.cost) > (current.fitness, candidate.cost)
                } else {
                    // 枠が制約なので、同性能なら大きい方を投入する
                    (candidate.fitness, candidate.cost) > (current.fitness, current.cost)
                }
            });
            if better {
                *slot = Some(candidate);
            }
        }
    }

    // 1 枠あたり予算に収まる候補を優先し、無ければ予算超過でも買える候補を使う
    best.or(best_over_budget)
}

/// 積荷を目標へ届けられるか。
///
/// 降車の可否はゲームのルール（`can_unload_from_terrain`）が決めており、
/// 艦船は港か浅瀬に接岸しないと積荷を降ろせない。海上に浮いたまま
/// 隣のマスへ降ろすことはできないので、「目標の隣まで行けるか」で
/// 判定しても渡洋作戦の成否とは対応しない。そこで
///   (1) 輸送自身が到達でき、かつ降車が許される揚陸地点があり、
///   (2) その隣接マスから積荷が自力で目標まで行ける
/// マスが存在するかどうかを見る。
///
/// 地形ルールは `can_unload_from_terrain` に、隣接の定義は
/// `map.get_adjacent` に委ねるため、特定のマップやトポロジーに依存しない。
fn can_deliver_cargo(
    map: &Map,
    registry: &MasterDataRegistry,
    ctx: &mut ReachCtx,
    from: &GridPosition,
    anchor: &GridPosition,
    transport_movement: MovementType,
    cargo_movement: MovementType,
) -> bool {
    let key = (
        transport_movement,
        cargo_movement,
        (from.x, from.y),
        (anchor.x, anchor.y),
    );
    if let Some(cached) = ctx.delivery.get(&key) {
        return *cached;
    }

    let mut result = false;
    'outer: for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            // 揚陸が許される地形か（艦船なら港・浅瀬のみ）
            if !can_unload_from_terrain(Some(transport_movement), Some(terrain)) {
                continue;
            }
            // 輸送自身がその揚陸地点まで行けるか
            if !ctx.is_reachable(map, registry, (from.x, from.y), (x, y), transport_movement) {
                continue;
            }
            // 降ろした先から積荷が目標へ行けるか。
            // `is_reachable` は積荷が進入できない地形を非連結として弾くので、
            // 降車先そのものの通行可否もここで同時に判定される。
            for (ax, ay) in map.get_adjacent(x, y) {
                if ctx.is_reachable(
                    map,
                    registry,
                    (ax, ay),
                    (anchor.x, anchor.y),
                    cargo_movement,
                ) {
                    result = true;
                    break 'outer;
                }
            }
        }
    }

    ctx.delivery.insert(key, result);
    result
}

/// `from` にいる（あるいはそこで生産される）ユニットが、この作戦へ投入できるか。
///
/// 成立するのは次のいずれか。
/// (1) 自力で作戦地点まで到達できる
/// (2) それを積める輸送ユニットを空き施設で生産でき、その輸送が積荷を目標へ揚陸できる
///
/// この関数は **購入候補の採用判定（`slot_fitness`）と既存戦力の計上（台帳）の
/// 両方から呼ばれなければならない**。片側だけ条件を変えると「その枠を埋められるのに
/// 充足としては数えられない」ユニットが生まれ、要求が永久に減らずに同じユニットを
/// 買い続けるラチェットになる。
/// 判定はユニット名ではなく能力（`max_cargo` / `loadable_unit_types`）で行う。
fn can_join_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: &GridPosition,
    requires_transport: bool,
    from: &GridPosition,
    stats: &UnitStats,
) -> bool {
    // 自力で作戦地点まで到達できるか
    if ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (from.x, from.y),
        (anchor.x, anchor.y),
        stats.movement_type,
    ) {
        return true;
    }
    if !requires_transport {
        return false;
    }
    // 空き施設で生産できる輸送ユニットの候補を先に洗い出す（借用を分離するため）
    let carriers: Vec<(GridPosition, MovementType)> = scan
        .free_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(|(unit_type, carrier)| {
                    carrier.max_cargo > 0
                        && carrier.loadable_unit_types.contains(&stats.unit_type)
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, carrier)| (*facility, carrier.movement_type))
        })
        .collect();

    carriers.into_iter().any(|(facility, movement_type)| {
        can_deliver_cargo(
            &scan.map,
            &scan.master_data,
            ctx,
            &facility,
            anchor,
            movement_type,
            stats.movement_type,
        )
    })
}

/// ユニットが指定枠にどれだけ適合するかを返す。適合しない場合は `None`。
fn slot_fitness(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    facility: &GridPosition,
    stats: &UnitStats,
) -> Option<f32> {
    // 施設から作戦地点まで自力で到達できるか
    let self_deployable = ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (facility.x, facility.y),
        (op.anchor.x, op.anchor.y),
        stats.movement_type,
    );

    match kind {
        SlotKind::Capture => {
            if !stats.can_capture {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 「輸送枠が立っている」だけでは運搬が成立する保証にならない。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            Some(1.0)
        }
        SlotKind::Transport => {
            if stats.max_cargo == 0 {
                return None;
            }
            // 「占領ユニットを積める」だけでは足りない。
            // その積荷を実際に目標へ揚陸できるところまで確かめる。
            // ここを緩めると運用の当てがない輸送を買い続けることになり、
            // 逆に目標マス自体への到達を求めると艦船が永久に候補から外れる。
            let deliverable = scan
                .available_types
                .iter()
                .filter(|(unit_type, cargo)| {
                    cargo.can_capture && stats.loadable_unit_types.contains(unit_type)
                })
                .map(|(_, cargo)| cargo.movement_type)
                .collect::<Vec<_>>()
                .into_iter()
                .any(|cargo_movement| {
                    can_deliver_cargo(
                        &scan.map,
                        &scan.master_data,
                        ctx,
                        facility,
                        &op.anchor,
                        stats.movement_type,
                        cargo_movement,
                    )
                });
            if !deliverable {
                return None;
            }
            Some(stats.max_cargo as f32)
        }
        SlotKind::Intercept => {
            if op.unreachable_threats.is_empty() {
                return None;
            }
            // 迎撃には「その脅威に届く」ことと「自力で現地へ行ける」ことの両方が要る。
            // 対空戦車が海を渡れないために選ばれないのは、この 2 条件の帰結。
            if !self_deployable {
                return None;
            }
            let indices: Vec<usize> = (0..op.unreachable_threats.len()).collect();
            let value = marginal_coverage(
                stats,
                stats.cost as f32,
                &op.unreachable_threats,
                &indices,
                &scan.damage_chart,
            );
            if value <= 0.0 { None } else { Some(value) }
        }
        SlotKind::Combat => {
            if stats.can_capture || stats.max_cargo > 0 {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            // 増援予測は撃破予算の上限には使えるが、未観測の敵兵種までは決められない。
            // ここで機動力などの汎用点へ退避すると、敵が0体の作戦が同じ兵種を全施設へ
            // 発注し続ける。具体的な残存脅威が無ければ限界価値も0として購入を止める。
            if op.reachable_threats.is_empty() {
                return None;
            }
            // 採点対象は「このユニットが実際に殴りに行ける敵」だけに限る。
            // 届かない敵まで含めた平均で採点すると、相性表の上でだけ強い
            // ユニット（海を渡れない対空戦車など）が延々と選ばれ、
            // 生産拠点に張り付いたまま敵の占領部隊を素通しにしてしまう。
            //
            // ただし揚陸される部隊は「上陸地点から殴りに行けるか」で採点する。
            // 施設からの到達性で採点すると、船で運べば戦える陸戦部隊が
            // すべて不適合となり、渡洋作戦の戦力が航空ユニットだけになる。
            let origin = if self_deployable {
                *facility
            } else {
                op.anchor
            };
            let engageable =
                reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats);
            if engageable.is_empty() {
                return None;
            }
            let value = marginal_coverage(
                stats,
                stats.cost as f32,
                &op.reachable_threats,
                &engageable,
                &scan.damage_chart,
            );
            if value <= 0.0 { None } else { Some(value) }
        }
    }
}

/// 敵 1 体を撃破枠の見積もりへ算入するときの重み付き価値。
///
/// 占領レースが進行中（未取得の拠点が残っている）の間は、敵の「拡張装置」＝
/// 自力で拠点を取れるユニットと、それを運べる輸送ユニットを素のコスト価値より重く数える。
/// これらは撃破しなければ盤面の収入を動かし続けるため、コストどおりに数えると
/// 撃破枠が立たず、局地戦の交換比で勝ちながら領地を明け渡すことになる。
/// 判定はユニット名ではなく能力（`can_capture` / `max_cargo`）で行うため、
/// マップやユニット構成に依存しない。
fn threat_value(unit: &UnitSnapshot, expansion_race_live: bool) -> u32 {
    if expansion_race_live && (unit.stats.can_capture || unit.stats.max_cargo > 0) {
        unit.value().saturating_mul(EXPANSION_THREAT_WEIGHT)
    } else {
        unit.value()
    }
}

/// 敵 1 体に対する対抗効率（与える価値 − 受ける価値）。
fn pair_value(unit: &UnitStats, enemy: &UnitStats, chart: &DamageChart) -> f32 {
    let dmg_out = best_damage(chart, unit.unit_type, enemy.unit_type);
    let dmg_in = best_damage(chart, enemy.unit_type, unit.unit_type);
    let out = dmg_out as f32 * enemy.cost as f32 / 100.0 * engagement_factor(unit, enemy);
    let inc = dmg_in as f32 * unit.cost as f32 / 100.0 * engagement_factor(enemy, unit);
    out - inc
}

/// 敵編成に対する対抗効率の平均。
fn counter_value(unit: &UnitStats, enemies: &[UnitStats], chart: &DamageChart) -> f32 {
    if enemies.is_empty() {
        return 0.0;
    }
    let total: f32 = enemies
        .iter()
        .map(|enemy| pair_value(unit, enemy, chart))
        .sum();
    total / enemies.len() as f32
}

/// `counter_value` の参照スライス版。到達可能な敵だけを抜き出して採点する用途で使う。
#[cfg(test)]
fn counter_value_refs(unit: &UnitStats, enemies: &[&UnitStats], chart: &DamageChart) -> f32 {
    if enemies.is_empty() {
        return 0.0;
    }
    let total: f32 = enemies
        .iter()
        .map(|enemy| pair_value(unit, enemy, chart))
        .sum();
    total / enemies.len() as f32
}

/// 主武器・副武器のうち有効な方のダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 射程と機動力の関係から、実際に交戦できる度合いを補正する係数。
fn engagement_factor(attacker: &UnitStats, defender: &UnitStats) -> f32 {
    let att_reach = attacker.max_movement + attacker.max_range;
    let def_reach = defender.max_movement + defender.max_range;
    if attacker.max_range > defender.max_range {
        if att_reach >= def_reach { 1.0 } else { 0.8 }
    } else if attacker.max_range < defender.max_range {
        0.5
    } else {
        1.0
    }
}

/// 見送り購入（資金を貯めて上位の候補を買う）を行うべきか。
///
/// 一括編成が必要な作戦に限り、いま買える候補より明確に適合度の高い候補が
/// `RESERVATION_PATIENCE_TURNS` 以内の収入で買えるなら、今ターンは生産しない。
fn should_defer_purchase(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    remaining_funds: u32,
    affordable_cost: u32,
) -> bool {
    if acquisition_mode(&op.facts) != AcquisitionMode::SquadPackage {
        return false;
    }
    if scan.my_income == 0 {
        return false;
    }
    // 輸送が要る作戦で輸送枠がまだ空いているのに他の枠を先に埋めるのは避ける
    let mut best_future: Option<(f32, u32)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost <= remaining_funds {
                continue;
            }
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
                continue;
            };
            let scaled = fitness * 1000.0 / stats.cost as f32;
            if best_future.is_none_or(|(current, _)| scaled > current) {
                best_future = Some((scaled, stats.cost));
            }
        }
    }
    let Some((future_fitness, future_cost)) = best_future else {
        return false;
    };
    // 現在買える候補の適合度
    let current_fitness = scan
        .available_types
        .iter()
        .filter(|(_, stats)| stats.cost <= remaining_funds && stats.cost == affordable_cost)
        .filter_map(|(_, stats)| {
            scan.free_facilities.iter().find_map(|(facility, terrain)| {
                if !scan.can_produce(*terrain, stats.unit_type) {
                    return None;
                }
                slot_fitness(scan, ctx, op, kind, facility, stats)
                    .map(|f| f * 1000.0 / stats.cost as f32)
            })
        })
        .fold(0.0f32, f32::max);

    if future_fitness <= current_fitness {
        return false;
    }
    let shortfall = future_cost.saturating_sub(remaining_funds);
    let turns_to_afford = shortfall.div_ceil(scan.my_income.max(1));
    turns_to_afford <= RESERVATION_PATIENCE_TURNS
}

/// 作戦が 1 つも立たない平時のフォールバック。
///
/// `GamePhase` ごとの理想構成は使わず、敵編成に対する対抗効率のみで選ぶ。
fn fallback_production(scan: &BoardScan, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let cheapest = scan
        .available_types
        .iter()
        .map(|(_, stats)| stats.cost)
        .filter(|cost| *cost > 0)
        .min()
        .unwrap_or(u32::MAX);
    // 資金に余裕がないうちは温存する
    if scan.funds < cheapest.saturating_mul(2) {
        return Vec::new();
    }

    let enemies: Vec<UnitStats> = scan
        .enemy_units
        .iter()
        .map(|unit| unit.stats.clone())
        .collect();

    let mut best: Option<(f32, GridPosition, UnitType)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost > scan.funds || stats.cost == 0
            {
                continue;
            }
            let base = if enemies.is_empty() {
                1.0
            } else {
                counter_value(stats, &enemies, &scan.damage_chart)
            };
            if base <= 0.0 {
                continue;
            }
            let score = base * 1000.0 / stats.cost as f32;
            if best.is_none_or(|(current, _, _)| score > current) {
                best = Some((score, *facility, *unit_type));
            }
        }
    }

    best.map(|(_, facility, unit_type)| ProduceUnitCommand {
        player_id,
        target_x: facility.x,
        target_y: facility.y,
        unit_type,
    })
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    fn flat_map(width: usize, height: usize) -> Map {
        Map {
            width,
            height,
            tiles: vec![Terrain::Plains; width * height],
            topology: GridTopology::Square,
        }
    }

    fn pos(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    /// 近接する拠点は 1 つの作戦にまとまり、離れた拠点は別作戦になる
    #[test]
    fn clusters_split_distant_property_groups() {
        let map = flat_map(20, 5);
        let positions = vec![pos(1, 1), pos(2, 1), pos(3, 2), pos(15, 1), pos(16, 2)];
        let clusters = cluster_positions(&positions, &map, OPERATION_CLUSTER_RADIUS);
        assert_eq!(clusters.len(), 2);
        let mut sizes: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![2, 3]);
    }

    /// 距離が閾値以内で連なっていれば、両端が離れていても 1 つの作戦になる
    #[test]
    fn clusters_chain_through_intermediate_properties() {
        let map = flat_map(20, 5);
        let positions = vec![pos(1, 1), pos(4, 1), pos(7, 1), pos(10, 1)];
        let clusters = cluster_positions(&positions, &map, OPERATION_CLUSTER_RADIUS);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 4);
    }

    /// 到達ターン数は距離を移動力で割り上げた値になる
    #[test]
    fn eta_is_distance_divided_by_movement() {
        let map = flat_map(20, 20);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(6, 0), 3), 2);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(7, 0), 3), 3);
        // 移動力 0 でも 0 除算しない
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(2, 0), 0), 2);
    }

    /// 等距離の敵も複数作戦へ重複計上せず、決定的に1作戦へ帰属する。
    #[test]
    fn equidistant_enemy_belongs_to_exactly_one_operation() {
        let map = flat_map(5, 3);
        let anchors = vec![pos(0, 1), pos(4, 1)];
        assert_eq!(nearest_anchor_index(&map, &pos(2, 1), 1, &anchors), Some(0));
    }

    /// テスト用のユニット諸元。射程はすべて 0 なので `engagement_factor` は 1.0 になる
    fn stats(unit_type: UnitType, cost: u32) -> UnitStats {
        UnitStats {
            unit_type,
            cost,
            ..UnitStats::mock()
        }
    }

    fn snapshot(stats: UnitStats, hp: u32) -> UnitSnapshot {
        UnitSnapshot {
            pos: pos(0, 0),
            stats,
            hp,
            free_cargo: 0,
        }
    }

    /// 占領レース中は、敵の占領ユニットと輸送ユニットを素のコスト価値より重く数える
    #[test]
    fn expansion_units_are_weighted_while_the_capture_race_is_live() {
        let capturer = snapshot(
            UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1000)
            },
            100,
        );
        let transport = snapshot(
            UnitStats {
                max_cargo: 1,
                ..stats(UnitType::TransportHelicopter, 5000)
            },
            100,
        );

        assert_eq!(
            threat_value(&capturer, true),
            1000 * EXPANSION_THREAT_WEIGHT
        );
        assert_eq!(
            threat_value(&transport, true),
            5000 * EXPANSION_THREAT_WEIGHT
        );

        // 取れる拠点が尽きて占領レースが終われば、重み付けもなくなる
        assert_eq!(threat_value(&capturer, false), 1000);
        assert_eq!(threat_value(&transport, false), 5000);
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
    fn strait_map(landing: Option<Terrain>) -> Map {
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

    /// 揚陸地点（港・浅瀬）が対岸にあれば、艦船は陸上ユニットを目標へ届けられる
    #[test]
    fn ship_can_deliver_land_cargo_through_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(Some(Terrain::Shoal));
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 対岸に接岸できる地形が無ければ、隣接マスが陸地でも積荷は降ろせない
    ///
    /// 「目標の隣まで行けるか」で判定すると、海に浮いたままの艦船が
    /// 陸へ積荷を降ろせることになってしまうため、この区別が必要。
    #[test]
    fn ship_cannot_deliver_land_cargo_without_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        // 対岸の陸地 (7,1) は海 (6,1) と隣接しているが、
        // 艦船は海の上では降車できないので不成立。
        assert!(!can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 自陣側の港からでも、自陣の陸地が目標なら当然届けられる（退行検出用）
    #[test]
    fn ship_can_deliver_cargo_back_to_its_own_shore() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(0, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 艦船以外の輸送（空輸など）は地形に縛られず、どこへでも降ろせる
    #[test]
    fn air_transport_is_not_restricted_by_landing_terrain() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Air,
            MovementType::Infantry,
        ));
    }

    /// 占領も輸送もできない戦闘ユニットは、レース中でも重み付けされない
    #[test]
    fn plain_combat_units_are_never_weighted() {
        let tank = snapshot(stats(UnitType::Tank, 7000), 100);
        assert_eq!(threat_value(&tank, true), 7000);
        assert_eq!(threat_value(&tank, false), 7000);
    }

    /// 脅威価値は HP を加味した残存戦力で数える
    #[test]
    fn threat_value_scales_with_remaining_hp() {
        let half_dead = snapshot(
            UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1000)
            },
            50,
        );
        assert_eq!(half_dead.value(), 500);
        assert_eq!(
            threat_value(&half_dead, true),
            500 * EXPANSION_THREAT_WEIGHT
        );
    }

    /// 撃破枠の採点は「そのユニットが実際に殴りに行ける敵」だけで行わないと、
    /// 届かない敵まで含めた平均のせいで、盤面に触れられないユニットが候補として残る。
    #[test]
    fn counter_value_ignores_enemies_the_unit_cannot_reach() {
        let mut chart = DamageChart::new();
        // 対空ユニットは航空ユニットに滅法強く、歩兵にはほとんど通らない
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 5);

        let anti_air = stats(UnitType::AntiAir, 8000);
        let bcopter = stats(UnitType::Bcopters, 9000);
        let infantry = stats(UnitType::Infantry, 1000);

        // 海の向こうのヘリまで平均に混ぜると正の値になり、候補として生き残ってしまう
        let with_unreachable = counter_value(&anti_air, &[bcopter, infantry.clone()], &chart);
        assert!(with_unreachable > 0.0);

        // 実際に届く相手（上陸してくる歩兵）だけで採点すれば有効打がなく脱落する
        let engageable_only = counter_value_refs(&anti_air, &[&infantry], &chart);
        assert!(engageable_only <= 0.0);
    }

    /// 到達できる敵が 1 体もいなければ採点対象がなく、枠を埋める資格もない
    #[test]
    fn counter_value_of_an_empty_engageable_set_is_zero() {
        let chart = DamageChart::new();
        let anti_air = stats(UnitType::AntiAir, 8000);
        assert_eq!(counter_value_refs(&anti_air, &[], &chart), 0.0);
    }

    /// 同じ航空脅威を覆い切った後は、次の対空ユニットより未対処の地上脅威への
    /// 対抗ユニットが優先される。平均相性のままではこの切替が起きない。
    #[test]
    fn marginal_coverage_moves_from_covered_air_threat_to_ground_threat() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 20);
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);

        let anti_air = stats(UnitType::AntiAir, 8000);
        let tank = stats(UnitType::Tank, 7000);
        let mut threats = vec![
            ThreatTarget {
                stats: stats(UnitType::Bcopters, 8000),
                position: pos(1, 0),
                remaining_value: 8000.0,
                priority_weight: 1.0,
            },
            ThreatTarget {
                stats: stats(UnitType::Infantry, 7000),
                position: pos(2, 0),
                remaining_value: 7000.0,
                priority_weight: 1.0,
            },
        ];
        let indices = vec![0, 1];

        assert!(
            marginal_coverage(&anti_air, 8000.0, &threats, &indices, &chart)
                > marginal_coverage(&tank, 7000.0, &threats, &indices, &chart)
        );
        apply_coverage(&anti_air, 8000.0, &mut threats, &indices, &chart);

        assert_eq!(threats[0].remaining_value, 0.0);
        assert_eq!(
            marginal_coverage(&anti_air, 8000.0, &threats, &indices, &chart),
            0.0
        );
        assert!(marginal_coverage(&tank, 7000.0, &threats, &indices, &chart) > 0.0);
    }

    /// 戦略的重要度は候補の優先順位だけを上げ、必要な対抗戦力を水増ししない。
    #[test]
    fn strategic_weight_does_not_multiply_remaining_combat_value() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::TransportHelicopter, 120);
        chart.insert_damage(UnitType::TransportHelicopter, UnitType::AntiAir, 0);
        let anti_air = stats(UnitType::AntiAir, 8000);
        let mut threats = vec![ThreatTarget {
            stats: stats(UnitType::TransportHelicopter, 8000),
            position: pos(1, 0),
            remaining_value: 8000.0,
            priority_weight: 2.0,
        }];

        let covered = apply_coverage(&anti_air, 8000.0, &mut threats, &[0], &chart);

        assert_eq!(covered, 16000.0);
        assert_eq!(threats[0].remaining_value, 0.0);
        assert_eq!(
            marginal_coverage(&anti_air, 8000.0, &threats, &[0], &chart),
            0.0
        );
    }

    /// テスト用の作戦。枠の充足状況だけを見たいので敵情報は空にしておく。
    fn operation(kind: OperationKind, slots: OperationSlots, filled: OperationSlots) -> Operation {
        Operation {
            kind,
            anchor: pos(0, 0),
            facts: OperationFacts::default(),
            slots,
            filled,
            unreachable_threats: Vec::new(),
            reachable_threats: Vec::new(),
        }
    }

    /// 要求が青天井の撃破枠は、上限を持つ前提条件の枠を飢えさせてはならない
    ///
    /// 撃破枠の要求は「自軍が投入できる資金」そのものなので、資金の何倍にもなる。
    /// 未充足率は要求量で正規化されるため、何体買っても 1.0 からほとんど下がらない。
    /// 未充足率を枠の優先順位より先に見ると、撃破枠が恒久的に「最も飢えた枠」となり、
    /// 半分埋まった輸送枠（＝揚陸の足回り）へは 2 度と資金が回らなくなる。
    #[test]
    fn an_unbounded_slot_does_not_starve_bounded_prerequisite_slots() {
        // 輸送枠は半分充足（未充足率 0.5）、撃破枠は資金規模の要求でほぼ未充足（≒1.0）
        let ops = vec![operation(
            OperationKind::Capture,
            OperationSlots {
                transport_slots: 4,
                destroy_budget: 150_000,
                ..OperationSlots::default()
            },
            OperationSlots {
                transport_slots: 2,
                destroy_budget: 8_000,
                ..OperationSlots::default()
            },
        )];

        // 未充足率だけで選ぶと撃破枠が勝ってしまうことを、前提として確かめておく
        assert!(
            ops[0].slots.deficit_ratio(SlotKind::Combat, &ops[0].filled)
                > ops[0]
                    .slots
                    .deficit_ratio(SlotKind::Transport, &ops[0].filled)
        );

        assert_eq!(most_starved_slot(&ops), Some((0, SlotKind::Transport)));
    }

    /// 海峡マップ上に、母港の輸送艦 1 隻だけを置いた盤面を作る。
    ///
    /// 対岸 (8,1) を獲りにいく作戦から見て、この輸送艦は
    /// 「距離では母港側が最寄り」だが「積荷を対岸へ届けられる」という位置関係になる。
    fn strait_scan() -> BoardScan {
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
            map: strait_map(Some(Terrain::Shoal)),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart: DamageChart::new(),
            funds: 20000,
            free_facilities: vec![(pos(1, 1), Terrain::Port)],
            available_types: vec![
                (UnitType::Infantry, infantry),
                (UnitType::Lander, lander.clone()),
            ],
            // 母港 (1,1) に停泊したままの輸送艦。空き搭載スロット 2。
            my_units: vec![UnitSnapshot {
                pos: pos(1, 1),
                stats: lander,
                hp: 100,
                free_cargo: 2,
            }],
            enemy_units: Vec::new(),
            my_properties: vec![pos(1, 1)],
            open_properties: vec![pos(8, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            my_income: 1000,
        }
    }

    /// 輸送台帳は「最寄り作戦」ではなく「その作戦へ届けられるか」で数える
    ///
    /// 輸送ユニットは自軍港湾に生まれて港に留まるため、位置で仕分けると常に
    /// 母港に近い作戦の台帳に載る。渡洋する作戦側の空き搭載スロットは永久に 0 となり、
    /// 輸送枠の要求が減らないまま揚陸艇を延々と買い増すラチェットになる。
    #[test]
    fn transports_are_ledgered_by_delivery_ability_not_proximity() {
        let scan = strait_scan();
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        // 母港側と対岸側、2 つの作戦地点がある盤面
        let anchors = vec![pos(0, 1), pos(8, 1)];

        // 前提: 母港の輸送艦は距離では母港側の作戦が最寄りである
        assert!(
            eta_turns(&scan.map, &pos(1, 1), &anchors[0], 6)
                < eta_turns(&scan.map, &pos(1, 1), &anchors[1], 6)
        );

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &[anchors[1]],
            3,
            0.5,
        );

        // それでも「対岸へ積荷を届けられる」以上、渡洋作戦の台帳に載らねばならない
        assert_eq!(overseas.facts.available_free_cargo_slots, 2);
    }

    /// 平地マップに工場 3 基と未取得拠点を置いた、生産ループ検証用の盤面。
    ///
    /// 空き施設を複数持たせることで「同一ターン内に複数施設へ発注が飛ぶ」状況を作り、
    /// その内訳がトレースに残るかを確かめられるようにする。
    fn multi_factory_scan() -> BoardScan {
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
            map: flat_map(9, 5),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart: DamageChart::new(),
            funds: 20000,
            free_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            available_types: vec![(UnitType::Infantry, infantry), (UnitType::Tank, tank)],
            my_units: Vec::new(),
            enemy_units: Vec::new(),
            my_properties: vec![pos(1, 2)],
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            my_income: 1000,
        }
    }

    /// 航空・地上の未対処脅威と工場3基を持つ、限界価値の統合テスト盤面。
    fn mixed_threat_multi_factory_scan() -> BoardScan {
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
            map: flat_map(9, 5),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart,
            funds: 17000,
            free_facilities: vec![
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
                    pos: pos(6, 1),
                    stats: stats(UnitType::Bcopters, 8000),
                    hp: 100,
                    free_cargo: 0,
                },
                UnitSnapshot {
                    pos: pos(6, 3),
                    stats: infantry,
                    hp: 100,
                    free_cargo: 0,
                },
            ],
            my_properties: vec![pos(1, 2)],
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            my_income: 1000,
        }
    }

    /// 同一手番の全施設を同じ残存脅威台帳で計画し、対空の次に地上対抗へ切り替える。
    #[test]
    fn multi_factory_plan_switches_after_air_threat_is_covered() {
        let scan = mixed_threat_multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0));
        let combat_types: Vec<UnitType> = commands
            .iter()
            .map(|command| command.unit_type)
            .filter(|unit_type| !matches!(unit_type, UnitType::Infantry))
            .collect();

        assert_eq!(
            combat_types,
            vec![UnitType::AntiAir, UnitType::Tank],
            "commands={commands:?}, trace={trace:?}"
        );
    }

    /// 増援予算だけでは敵兵種を特定できないため、敵未観測の撃破枠から汎用兵を作らない。
    #[test]
    fn combat_slot_without_observed_threat_does_not_produce() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 10_000;
        scan.enemy_production_slots = 1;

        let (commands, trace) = plan_production(&scan, PlayerId(0));

        assert!(
            commands
                .iter()
                .all(|command| command.unit_type != UnitType::Tank)
        );
        assert!(trace.steps.iter().all(|step| {
            step.slot_kind != SlotKind::Combat
                || !matches!(step.decision, ProductionDecision::Produced { .. })
        }));
    }

    /// 生産トレースは、発行した命令 1 件ごとに「どの作戦のどの枠から出たか」を残す
    ///
    /// 「同一ターン内に同じユニットが全施設へ発注される」現象を切り分けるには、
    /// 発注とトレースが 1 対 1 で対応していなければならない。ズレた時点で
    /// 診断そのものが無意味になるため、記録専用であるという不変条件をここで固定する。
    #[test]
    fn production_trace_attributes_every_command_to_a_slot() {
        let scan = multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0));

        // 作戦が立つ盤面なので fallback には落ちない
        assert!(!trace.fallback);
        assert!(!trace.operations.is_empty());
        assert!(!commands.is_empty());
        assert_eq!(trace.funds, scan.funds);
        assert_eq!(trace.free_facility_count, scan.free_facilities.len());

        // 発注は 1 件残らず Produced ステップとして記録される
        let produced: Vec<_> = trace
            .steps
            .iter()
            .filter_map(|step| match &step.decision {
                ProductionDecision::Produced {
                    unit_type,
                    cost,
                    facility,
                } => Some((*unit_type, *cost, *facility)),
                _ => None,
            })
            .collect();
        assert_eq!(produced.len(), commands.len());
        for (command, (unit_type, _, facility)) in commands.iter().zip(produced.iter()) {
            assert_eq!(command.unit_type, *unit_type);
            assert_eq!(command.target_x, facility.x);
            assert_eq!(command.target_y, facility.y);
            // 発注先は必ず空き施設のいずれか
            assert!(scan.free_facilities.iter().any(|(f, _)| f == facility));
        }

        // 資金の収支が合うこと（余剰資金の積み上がりを測る土台になる）
        let spent: u32 = produced.iter().map(|(_, cost, _)| *cost).sum();
        assert_eq!(trace.leftover_funds, scan.funds - spent);

        // 種別ごとの体数集計も命令と一致する（工場数への張り付きを数える入口）
        assert_eq!(
            trace.produced_counts().values().sum::<usize>(),
            commands.len()
        );
    }

    /// 届けられない作戦地点の台帳には載せない（二重計上の歯止め）
    #[test]
    fn transports_are_not_ledgered_for_unreachable_anchors() {
        let mut scan = strait_scan();
        // 接岸できる地形を消すと、艦船は対岸へ陸上ユニットを降ろせなくなる
        scan.map = strait_map(None);
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let anchors = vec![pos(0, 1), pos(8, 1)];

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &[anchors[1]],
            3,
            0.5,
        );

        assert_eq!(overseas.facts.available_free_cargo_slots, 0);
    }

    /// 前提条件どうしの競合では、作戦の優先度が枠の優先順位より上位に効く
    #[test]
    fn operation_priority_outranks_slot_priority_among_prerequisites() {
        let ops = vec![
            // 占領作戦の輸送枠（枠としては先だが、作戦としては後回し）
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 4,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 防衛作戦の護衛枠（枠としては最後だが、作戦が最優先）。
            // 護衛は「敵の接触までに要る体数」で有限なので前提条件側に属する。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    escort_units: 2,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }

    /// 最優先作戦の撃破枠が、後回し作戦の前提条件を飢えさせてはならない
    ///
    /// 撃破枠の要求は青天井なので、作戦優先度で先に見てしまうと自陣の防衛作戦が
    /// 全額を吸い、渡洋作戦には輸送も占領要員も 1 体も回らない（＝引きこもる）。
    /// 前提条件は作戦をまたいで先に満たす。
    #[test]
    fn a_top_priority_destroy_budget_does_not_starve_a_lower_priority_prerequisite() {
        let ops = vec![
            // 自陣の防衛作戦。撃破枠は資金規模の青天井要求。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    destroy_budget: 150_000,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 渡洋する占領作戦。輸送が無ければ 1 歩も進めない。
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 2,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Transport)));
    }

    /// 余剰の配分は作戦の優先度ではなく未充足率で決める
    ///
    /// 撃破要求は既に前線ごとの分担比で割ってあるので、未充足率で選び続ければ
    /// 資金は各前線の分担比どおりに配分される。
    #[test]
    fn residual_funds_follow_the_deficit_not_the_operation_rank() {
        let ops = vec![
            // 最優先の防衛作戦。撃破枠はほぼ充足済み。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    destroy_budget: 10_000,
                    ..OperationSlots::default()
                },
                OperationSlots {
                    destroy_budget: 9_000,
                    ..OperationSlots::default()
                },
            ),
            // 後回しの占領作戦。撃破枠は手つかず。
            operation(
                OperationKind::Capture,
                OperationSlots {
                    destroy_budget: 10_000,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }
}
