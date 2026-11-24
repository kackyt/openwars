//! 小規模マップ向けの単一統合生産評価（シミュレーション不使用）。
//!
//! 占領（Capture）・直接戦闘／対空／航空（Combat Direct）・間接火網（Indirect）を
//! 共通の評価スケール（共有 `rem_hp` ＋ 占領時間割引収入 ＋ スクリーン維持）へ乗せ、
//! 全空き生産施設・残存資金の範囲でビームサーチにより一括決定する。

mod composition;
mod reach;
mod scoring;
mod threats;

#[cfg(test)]
mod tests;

pub(in crate::ai::v4) use scoring::plan_unified_production;

use crate::ai::v4::BoardScan;
use crate::collections::HashMap;
use crate::components::{Ammo, GridPosition, PlayerId, Property};
use crate::resources::master_data::UnitName;
use crate::resources::{Terrain, UnitType};
use bevy_ecs::prelude::{Entity, World};

/// `BoardScan` を拡張せずに統合生産評価へ渡す追加盤面情報（自軍/敵物件と実残弾）。
#[derive(Debug, Clone, Default)]
pub(in crate::ai::v4) struct UnifiedBoardExtras {
    pub(in crate::ai::v4) friendly_properties: Vec<(GridPosition, Terrain)>,
    pub(in crate::ai::v4) enemy_properties: Vec<(GridPosition, Terrain)>,
    pub(in crate::ai::v4) unit_ammo: HashMap<Entity, (u32, u32)>,
}

/// `World` から統合生産評価用の所有物件一覧と実残弾を収集する。
pub(in crate::ai::v4) fn collect_board_extras(
    world: &mut World,
    player_id: PlayerId,
) -> UnifiedBoardExtras {
    let mut extras = UnifiedBoardExtras::default();
    let mut prop_query = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in prop_query.iter(world) {
        match prop.owner_id {
            Some(owner) if owner == player_id => {
                extras.friendly_properties.push((*pos, prop.terrain));
            }
            Some(_) => {
                extras.enemy_properties.push((*pos, prop.terrain));
            }
            None => {}
        }
    }
    let mut ammo_query = world.query::<(Entity, &Ammo)>();
    for (entity, ammo) in ammo_query.iter(world) {
        extras.unit_ammo.insert(entity, (ammo.ammo1, ammo.ammo2));
    }
    extras
}

/// 武器1・武器2の射程をホットループ外で事前解決したキャッシュ。
#[derive(Debug, Clone, Copy)]
pub(super) struct WeaponProfile {
    pub(super) w1_range: Option<(u32, u32)>,
    pub(super) w2_range: Option<(u32, u32)>,
}

/// `BoardScan` 上に出現する全 `UnitType` に対する武器プロファイル表。
pub(super) struct WeaponProfileTable {
    entries: HashMap<UnitType, WeaponProfile>,
}

impl WeaponProfileTable {
    /// `BoardScan` が参照する全ユニット種別の武器射程を事前解決する。
    pub(super) fn from_scan(scan: &BoardScan) -> Self {
        let mut entries = HashMap::default();
        for (ut, stats) in &scan.available_types {
            entries.insert(*ut, resolve_weapon_profile(scan, *ut, stats));
        }
        for u in scan.my_units.iter().chain(scan.enemy_units.iter()) {
            entries
                .entry(u.stats.unit_type)
                .or_insert_with(|| resolve_weapon_profile(scan, u.stats.unit_type, &u.stats));
        }
        Self { entries }
    }

    #[inline]
    pub(super) fn get(&self, unit_type: UnitType) -> Option<WeaponProfile> {
        self.entries.get(&unit_type).copied()
    }

    #[inline]
    pub(super) fn has_any_weapon(&self, unit_type: UnitType) -> bool {
        self.entries
            .get(&unit_type)
            .is_some_and(|p| p.w1_range.is_some() || p.w2_range.is_some())
    }

    /// 全武装の最小射程が 2 以上（移動後攻撃不可の間接ユニット）か。
    #[inline]
    pub(super) fn is_indirect(&self, unit_type: UnitType) -> bool {
        let Some(p) = self.entries.get(&unit_type) else {
            return false;
        };
        let has_weapon = p.w1_range.is_some() || p.w2_range.is_some();
        has_weapon
            && p.w1_range.is_none_or(|(rmin, _)| rmin > 1)
            && p.w2_range.is_none_or(|(rmin, _)| rmin > 1)
    }
}

fn resolve_weapon_profile(
    scan: &BoardScan,
    unit_type: UnitType,
    stats: &crate::components::UnitStats,
) -> WeaponProfile {
    let record = scan
        .master_data
        .get_unit(&UnitName(unit_type.as_str().to_owned()));
    if let Some(rec) = record {
        let w1_range = rec
            .weapon1
            .as_ref()
            .and_then(|name| scan.master_data.weapons.get(&UnitName(name.clone())))
            .map(|w| (w.range_min, w.range_max));
        let w2_range = rec
            .weapon2
            .as_ref()
            .and_then(|name| scan.master_data.weapons.get(&UnitName(name.clone())))
            .map(|w| (w.range_min, w.range_max));
        WeaponProfile { w1_range, w2_range }
    } else {
        // マスターデータ未登録のモックユニット向けフォールバック
        let fallback = (stats.max_range > 0).then_some((stats.min_range.max(1), stats.max_range));
        WeaponProfile {
            w1_range: fallback,
            w2_range: None,
        }
    }
}
