//! 攻撃1回の交戦見積り。自拠点防衛と施設攻略の戦闘割当で同じ計算を使う。
//!
//! 兵種名の相性表ではなく、実際の武器選択・HP・弾薬・地形防御から
//! 「与えるダメージ」と「受ける反撃による損失額」を求める。

use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map};
use crate::systems::combat::get_detailed_expected_damage;

/// 交戦する1ユニットの現在値。
#[derive(Clone, Copy)]
pub(crate) struct Combatant<'a> {
    pub stats: &'a UnitStats,
    pub hp: u32,
    pub ammo: (u32, u32),
}

/// 1回の攻撃の見積り。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exchange {
    /// 防御側へ与えるHP。
    pub damage: u32,
    /// 反撃で失う攻撃側の価値（失うHP×価格/100）。間接攻撃は反撃を受けない。
    pub loss: u32,
}

/// `firing_position` から `target` にいる防御側を攻撃した場合の見積り。
/// 武器が届かない、または有効打にならない場合は `None`。
pub(crate) fn expected_exchange(
    map: &Map,
    data: &MasterDataRegistry,
    chart: &DamageChart,
    attacker: Combatant<'_>,
    defender: Combatant<'_>,
    firing_position: GridPosition,
    target: GridPosition,
) -> Option<Exchange> {
    let target_bonus = data.get_terrain_defense_bonus(map.get_terrain(target.x, target.y)?);
    let range = map.distance(firing_position.x, firing_position.y, target.x, target.y);
    let (damage, (_, _, indirect)) = get_detailed_expected_damage(
        attacker.stats,
        attacker.hp,
        attacker.ammo,
        defender.stats,
        target_bonus,
        range,
        data,
        chart,
        false,
    )?;
    if damage == 0 {
        return None;
    }
    let own_bonus =
        data.get_terrain_defense_bonus(map.get_terrain(firing_position.x, firing_position.y)?);
    let counter = if indirect {
        0
    } else {
        get_detailed_expected_damage(
            defender.stats,
            defender.hp,
            defender.ammo,
            attacker.stats,
            own_bonus,
            range,
            data,
            chart,
            true,
        )
        .map_or(0, |(damage, _)| damage)
    };
    Some(Exchange {
        damage,
        loss: counter.min(attacker.hp).saturating_mul(attacker.stats.cost) / 100,
    })
}
