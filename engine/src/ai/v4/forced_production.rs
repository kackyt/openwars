//! 診断用: 外部で指定した購入列を、V4 の生産判断の代わりにそのまま発行する。
//!
//! 「人間の購入を V4 に買わせ、部隊の動かし方は V4 のまま」で勝てるかを切り分けるための
//! 実験スイッチ。`OPENWARS_FORCED_PURCHASES`（購入列JSONのパス）と
//! `OPENWARS_FORCED_PLAYER`（対象プレイヤーID）の両方があるときだけ有効で、
//! 未設定時は何もしない（既定の挙動は変わらない）。
//!
//! JSON 形式: `{"<手番>": [[x, y, "Infantry"], ...], ...}`（兵種は `UnitType` の Debug 名）。
//! - 列に含まれる手番では、列の購入だけを発行し、V4 自身の生産判断は行わない。
//! - 指定施設が使えない（占有・未所有など）ときは、同じ兵種を生産できる自軍施設のうち
//!   指定施設に最も近いものへ置き換える。置き換え先も無ければその1件は買わない。
//! - 資金不足の購入は発行後に生産側で失敗し、そのまま捨てられる。
//! - 列の最後の手番より後は、通常の V4 生産へ戻る。

use crate::components::{GridPosition, PlayerId, Property};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{MatchState, UNIT_TYPE_MAP, UnitType};
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, HashSet};

/// 同じ手番に強制購入を一度だけ発行したことを覚える。生産は手番中に何度も呼ばれるため、
/// 2回目以降に同じ列を再発行して余分に買わないようにする。
#[derive(Resource, Debug, Default)]
struct ForcedPurchaseIssued {
    player_id: Option<PlayerId>,
    turn: u32,
}

/// 強制購入が有効な手番なら `Some(命令列)` を返す。無効なら `None`（通常の V4 生産へ）。
pub(in crate::ai::v4) fn forced_purchases(
    world: &mut World,
    player_id: PlayerId,
) -> Option<Vec<ProduceUnitCommand>> {
    let path = std::env::var("OPENWARS_FORCED_PURCHASES").ok()?;
    let target: u32 = std::env::var("OPENWARS_FORCED_PLAYER").ok()?.parse().ok()?;
    if player_id.0 != target {
        return None;
    }
    let plan: BTreeMap<u32, Vec<(usize, usize, String)>> =
        serde_json::from_str::<BTreeMap<String, Vec<(usize, usize, String)>>>(
            &std::fs::read_to_string(path).ok()?,
        )
        .ok()?
        .into_iter()
        .filter_map(|(turn, rows)| Some((turn.parse().ok()?, rows)))
        .collect();
    let turn = world.get_resource::<MatchState>()?.current_turn_number.0;
    // 列にない手番は、同じ手番の再呼び出しも含めて通常の V4 生産に任せる。
    let rows = plan.get(&turn)?;
    // 強制対象の手番では2回目以降の呼び出しで何も買わない。
    if world
        .get_resource::<ForcedPurchaseIssued>()
        .is_some_and(|issued| issued.player_id == Some(player_id) && issued.turn == turn)
    {
        return Some(Vec::new());
    }
    world.insert_resource(ForcedPurchaseIssued {
        player_id: Some(player_id),
        turn,
    });

    // 自軍所有の物件（生産可否は施設ごとに can_produce_at で判定する）
    let owned: Vec<GridPosition> = world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .filter(|(_, property)| property.owner_id == Some(player_id))
        .map(|(position, _)| *position)
        .collect();
    let topology = world.resource::<crate::resources::Map>().topology;

    world.resource_scope(|world, master_data: Mut<MasterDataRegistry>| {
        let mut used = HashSet::new();
        let mut commands = Vec::new();
        for (x, y, name) in rows {
            let Some(unit_type) = unit_type_by_debug_name(name) else {
                continue;
            };
            let requested = GridPosition { x: *x, y: *y };
            // 指定施設 → 近い順の代替施設、の順に生産できる最初の施設を使う。
            let mut candidates = owned.clone();
            candidates.sort_by_key(|p| {
                (
                    topology.distance((p.x, p.y), (requested.x, requested.y)),
                    p.y,
                    p.x,
                )
            });
            let site = candidates.into_iter().find(|site| {
                !used.contains(site)
                    && crate::systems::production::can_produce_at(
                        world,
                        player_id,
                        site.x,
                        site.y,
                        unit_type,
                        &master_data,
                    )
                    .is_ok()
            });
            if let Some(site) = site {
                used.insert(site);
                commands.push(ProduceUnitCommand {
                    player_id,
                    target_x: site.x,
                    target_y: site.y,
                    unit_type,
                });
            }
        }
        Some(commands)
    })
}

/// ログの兵種名（`UnitType` の Debug 名）から `UnitType` を引く。
fn unit_type_by_debug_name(name: &str) -> Option<UnitType> {
    UNIT_TYPE_MAP
        .iter()
        .map(|(unit_type, _)| *unit_type)
        .find(|unit_type| format!("{unit_type:?}") == name)
}
