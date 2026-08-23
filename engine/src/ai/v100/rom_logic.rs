//! Game Boy Wars Turbo Bank 2のAI判定を表す純粋な状態機械。
//!
//! このモジュールではOpenWarsの合法手生成を行わない。ROM側の兵種番号、走査順、
//! 状態値を型として保持し、`action`と`production`が境界でOpenWarsの盤面へ適用する。

use crate::ai::AiVersion;
use crate::components::PlayerId;
use crate::components::UnitStats;
use crate::resources::UnitType;
use bevy_ecs::prelude::{Entity, Resource};
use std::collections::{HashMap, HashSet};

/// ROMの部隊レコード先頭に入る陣営込み兵種番号から、陣営bitを除いた値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GbUnitKind(u8);

impl GbUnitKind {
    const INFANTRY: Self = Self(0x00);
    const MECH: Self = Self(0x02);
    const TANK_Z: Self = Self(0x04);
    const MD_TANK: Self = Self(0x06);
    const TANK: Self = Self(0x08);
    const ARTILLERY: Self = Self(0x0A);
    const HEAVY_SP_GUN: Self = Self(0x0C);
    const LIGHT_SP_GUN: Self = Self(0x0E);
    const MISSILES: Self = Self(0x10);
    const ANTI_AIR: Self = Self(0x12);
    const ROCKETS: Self = Self(0x14);
    const RECON: Self = Self(0x16);
    const SUPPLY_TRUCK: Self = Self(0x18);
    const HEAVY_FIGHTER: Self = Self(0x1A);
    const FIGHTER: Self = Self(0x1C);
    const BOMBER: Self = Self(0x1E);
    // 0x20はGB固有のレーダー輸送機。
    const BCOPTERS: Self = Self(0x22);
    const TRANSPORT_HELICOPTER: Self = Self(0x24);
    // 0x26はGB固有のスーパーミサイル。
    const BATTLESHIP: Self = Self(0x28);
    const CARRIER: Self = Self(0x2A);
    const LANDER: Self = Self(0x2C);
    // 0x2EはGB固有の潜水艦。

    /// ROM 2FDDの24件の兵種表を、名前・価格・移動力・燃料・武器索引で照合した対応。
    /// GB固有の3兵種はOpenWarsに存在しないため、この向きの変換には現れない。
    pub(crate) fn from_openwars(unit_type: UnitType) -> Option<Self> {
        match unit_type {
            UnitType::Infantry => Some(Self::INFANTRY),
            UnitType::Mech => Some(Self::MECH),
            UnitType::TankZ => Some(Self::TANK_Z),
            UnitType::MdTank => Some(Self::MD_TANK),
            UnitType::Tank => Some(Self::TANK),
            UnitType::Artillery => Some(Self::ARTILLERY),
            UnitType::HeavySpGun => Some(Self::HEAVY_SP_GUN),
            UnitType::LightSpGun => Some(Self::LIGHT_SP_GUN),
            UnitType::Missiles => Some(Self::MISSILES),
            UnitType::AntiAir => Some(Self::ANTI_AIR),
            UnitType::Rockets => Some(Self::ROCKETS),
            UnitType::Recon => Some(Self::RECON),
            UnitType::SupplyTruck => Some(Self::SUPPLY_TRUCK),
            UnitType::HeavyFighter => Some(Self::HEAVY_FIGHTER),
            UnitType::Fighter => Some(Self::FIGHTER),
            UnitType::Bomber => Some(Self::BOMBER),
            UnitType::Bcopters => Some(Self::BCOPTERS),
            UnitType::TransportHelicopter => Some(Self::TRANSPORT_HELICOPTER),
            UnitType::Battleship => Some(Self::BATTLESHIP),
            UnitType::Carrier => Some(Self::CARRIER),
            UnitType::Lander => Some(Self::LANDER),
        }
    }

    /// ROMの兵種表を走査するときの昇順キー。陣営bitは境界で付与しない。
    pub(crate) fn production_order(unit_type: UnitType) -> u8 {
        Self::from_openwars(unit_type)
            .expect("all OpenWars unit types have a GB counterpart")
            .0
    }

    /// ROM 51E1の`kind < 4`。
    pub(crate) fn increments_pickup_counter(unit_type: UnitType) -> bool {
        Self::production_order(unit_type) < 0x04
    }

    /// ROM 4A42の`4 <= kind < 0x1A`。
    pub(crate) fn increments_mobility_shortage_counter(unit_type: UnitType) -> bool {
        let kind = Self::production_order(unit_type);
        (0x04..0x1A).contains(&kind)
    }

    /// ROM 687Cの輸送集合判定距離。C69Eがこの値未満なら51BFは搭載探索を行わない。
    pub(crate) fn pickup_distance_threshold(unit_type: UnitType) -> u32 {
        match Self::from_openwars(unit_type) {
            Some(Self::INFANTRY) => 12,
            Some(Self::MECH) => 10,
            Some(Self::ARTILLERY) => 1,
            Some(Self::ROCKETS | Self::RECON | Self::SUPPLY_TRUCK) => 20,
            _ => 0xFF,
        }
    }

    /// ROM Bank 2 `6894`の兵種別影響値。
    ///
    /// 下位3bitは対空、bit 3〜5は対地・対艦の脅威寄与であり、`4320`は敵部隊の
    /// 射程内マスへ各成分を7で飽和加算する。ミッション3の通常移動は`4125`で
    /// 両成分の合計が最大となる到達可能マスを進軍目標に選ぶ。
    pub(crate) fn influence_weights(unit_type: UnitType) -> (u8, u8) {
        const TABLE: [u8; 24] = [
            0x09, 0x09, 0x19, 0x11, 0x10, 0x18, 0x10, 0x08, 0x03, 0x02, 0x08, 0x08, 0x00, 0x0B,
            0x0A, 0x18, 0x00, 0x10, 0x08, 0x24, 0x1B, 0x03, 0x09, 0x18,
        ];
        let kind = Self::production_order(unit_type);
        let packed = TABLE[usize::from(kind / 2)];
        (packed & 0x07, (packed >> 3) & 0x07)
    }

    fn is(self, expected: Self) -> bool {
        self == expected
    }
}

/// ROM 447Bが呼ぶ8本の部隊選択ルーチン。値がそのまま呼び出し順を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum ActorSelectionPass {
    Kind26 = 0,
    Kind18 = 1,
    Indirect = 2,
    MobileOnProperty = 3,
    OnProperty = 4,
    SpecialMobileKind = 5,
    UnassignedObjective = 6,
    MinimumObjectiveCost = 7,
}

/// ROM 449A〜461Eの各走査条件を、1部隊について最初に該当するpassへ変換する。
///
/// `on_property`は部隊レコード+6が`0x3D`未満というROM条件に対応する。
pub(crate) fn actor_selection_pass(
    stats: &UnitStats,
    on_property: bool,
    has_objective: bool,
) -> ActorSelectionPass {
    let kind = GbUnitKind::from_openwars(stats.unit_type);
    // ROM 449Aが最優先する0x26はスーパーミサイルであり、OpenWarsに対応兵種はない。
    if kind.is_some_and(|kind| kind.0 == 0x26) {
        return ActorSelectionPass::Kind26;
    }
    if kind.is_some_and(|kind| kind.is(GbUnitKind::SUPPLY_TRUCK)) {
        return ActorSelectionPass::Kind18;
    }
    // ROM 44F4は兵種表を読み、C4FDの下位nibbleが1の兵種を選ぶ。
    // OpenWars境界では「移動後に撃てない間接兵器」をこのカテゴリへ対応させる。
    if stats.min_range > 1 {
        return ActorSelectionPass::Indirect;
    }
    if on_property
        && kind.is_some_and(|kind| {
            kind.is(GbUnitKind::RECON) || kind.is(GbUnitKind::TRANSPORT_HELICOPTER)
        })
    {
        return ActorSelectionPass::MobileOnProperty;
    }
    if on_property {
        return ActorSelectionPass::OnProperty;
    }
    if kind.is_some_and(|kind| {
        kind.is(GbUnitKind::RECON)
            || kind.is(GbUnitKind::TRANSPORT_HELICOPTER)
            || kind.is(GbUnitKind::LANDER)
    }) {
        return ActorSelectionPass::SpecialMobileKind;
    }
    if !has_objective {
        return ActorSelectionPass::UnassignedObjective;
    }
    ActorSelectionPass::MinimumObjectiveCost
}

/// ROM 5FA8〜6026が自軍戦力比から選ぶ生産戦略値C6AD。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ProductionStrategy {
    Opening = 0,
    Advantage = 1,
    Balanced = 2,
    Disadvantage = 3,
}

impl ProductionStrategy {
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// ROM WRAM C6A6が選ぶ兵種価値表の組。
///
/// Bank 2 `5179`は通常時にシナリオプロファイルへ4を加えて表4〜7を読み、
/// 自軍首都へ敵が接近した時だけ加算せず表0〜3を読む。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RomEvaluationMode {
    #[default]
    Normal,
    Restricted,
}

#[derive(Debug, Clone, Copy, Default)]
struct RomPlayerState {
    production_strategy: Option<ProductionStrategy>,
    action_turn: Option<u32>,
    mobility_shortage_count: u8,
    pickup_candidate_count: u8,
    evaluation_mode: RomEvaluationMode,
    restricted_target: Option<UnitType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecialProductionMode {
    Mobility,
    Pickup,
}

/// WRAM C6ADに相当する陣営別の生産戦略状態。
#[derive(Resource, Default)]
pub(crate) struct RomAiState {
    by_player: HashMap<PlayerId, RomPlayerState>,
    pickup_eligible_by_player: HashMap<PlayerId, HashSet<Entity>>,
    observed_units_by_player: HashMap<PlayerId, HashSet<Entity>>,
}

impl RomAiState {
    /// ROM部隊レコード+12のbit 5を、新造・撃破された部隊に追従させる。
    /// 新造時のレコードはbit 5が立っており、以後は行動分岐が明示的に更新する。
    pub(crate) fn observe_units(
        &mut self,
        player_id: PlayerId,
        alive_units: impl IntoIterator<Item = Entity>,
    ) {
        let alive: HashSet<_> = alive_units.into_iter().collect();
        let observed = self.observed_units_by_player.entry(player_id).or_default();
        let eligible = self.pickup_eligible_by_player.entry(player_id).or_default();
        eligible.retain(|entity| alive.contains(entity));
        for entity in &alive {
            if !observed.contains(entity) {
                eligible.insert(*entity);
            }
        }
        *observed = alive;
    }

    /// ROM 51BF/52E1と4E4Dに対応し、輸送部隊の集合対象bitを更新する。
    pub(crate) fn set_pickup_eligible(
        &mut self,
        player_id: PlayerId,
        entity: Entity,
        eligible: bool,
    ) {
        let entries = self.pickup_eligible_by_player.entry(player_id).or_default();
        if eligible {
            entries.insert(entity);
        } else {
            entries.remove(&entity);
        }
    }

    pub(crate) fn pickup_eligible_units(&self, player_id: PlayerId) -> HashSet<Entity> {
        self.pickup_eligible_by_player
            .get(&player_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn set_production_strategy(
        &mut self,
        player_id: PlayerId,
        strategy: ProductionStrategy,
    ) {
        self.by_player
            .entry(player_id)
            .or_default()
            .production_strategy = Some(strategy);
    }

    pub(crate) fn begin_action_turn(&mut self, player_id: PlayerId, turn: u32) {
        let state = self.by_player.entry(player_id).or_default();
        if state.action_turn != Some(turn) {
            state.action_turn = Some(turn);
            state.mobility_shortage_count = 0;
            state.pickup_candidate_count = 0;
        }
    }

    /// ROM 4453〜4475と同じく、行動選択を始めるたびにC6A6相当値を上書きする。
    pub(crate) fn set_evaluation_mode(
        &mut self,
        player_id: PlayerId,
        mode: RomEvaluationMode,
        restricted_target: Option<UnitType>,
    ) {
        let state = self.by_player.entry(player_id).or_default();
        state.evaluation_mode = mode;
        state.restricted_target = restricted_target;
    }

    pub(crate) fn evaluation_mode_for(&self, player_id: PlayerId) -> RomEvaluationMode {
        self.by_player
            .get(&player_id)
            .map_or(RomEvaluationMode::Normal, |state| state.evaluation_mode)
    }

    pub(crate) fn restricted_target_for(&self, player_id: PlayerId) -> Option<UnitType> {
        self.by_player
            .get(&player_id)
            .and_then(|state| state.restricted_target)
    }

    pub(crate) fn record_pickup_candidate(&mut self, player_id: PlayerId) {
        let state = self.by_player.entry(player_id).or_default();
        state.pickup_candidate_count = state.pickup_candidate_count.saturating_add(1);
    }

    pub(crate) fn record_mobility_shortage(&mut self, player_id: PlayerId) {
        let state = self.by_player.entry(player_id).or_default();
        state.mobility_shortage_count = state.mobility_shortage_count.saturating_add(1);
    }

    pub(crate) fn production_counters(&self, player_id: PlayerId) -> (u8, u8) {
        self.by_player.get(&player_id).map_or((0, 0), |state| {
            (state.mobility_shortage_count, state.pickup_candidate_count)
        })
    }

    pub(crate) fn consume_pickup_candidates(&mut self, player_id: PlayerId, amount: u8) {
        let state = self.by_player.entry(player_id).or_default();
        state.pickup_candidate_count = state.pickup_candidate_count.saturating_sub(amount);
    }

    pub(crate) fn consume_mobility_shortages(&mut self, player_id: PlayerId, amount: u8) {
        let state = self.by_player.entry(player_id).or_default();
        state.mobility_shortage_count = state.mobility_shortage_count.saturating_sub(amount);
    }

    pub(crate) fn production_strategy_for(
        &self,
        player_id: PlayerId,
    ) -> Option<ProductionStrategy> {
        self.by_player
            .get(&player_id)
            .and_then(|state| state.production_strategy)
    }
}

/// ROM Bank 2 `6067`は生産処理へ入るたびにC6A4/C6A5を読み直す。
/// 特殊兵種の生産でカウンタが減った後は、同じ手番でも次の生産判断を通常走査へ戻す。
pub(crate) fn special_production_mode(
    mobility_shortages: u8,
    pickup_candidates: u8,
) -> Option<SpecialProductionMode> {
    if mobility_shortages >= 2 {
        Some(SpecialProductionMode::Mobility)
    } else if pickup_candidates >= 3 {
        Some(SpecialProductionMode::Pickup)
    } else {
        None
    }
}

pub(crate) fn production_strategy(own_share_percent: u32) -> ProductionStrategy {
    if own_share_percent >= 60 {
        ProductionStrategy::Advantage
    } else if own_share_percent >= 40 {
        ProductionStrategy::Balanced
    } else {
        ProductionStrategy::Disadvantage
    }
}

/// ROM Bank 2 `6257`〜`630F`が部隊レコード+12の下位2bitへ設定する任務状態。
///
/// `same_kind_counts`は同兵種の生存部隊を状態0〜3ごとに数えた値、
/// `production_limit`はBank 0 `0B11`が返す現在戦略の保有上限である。
pub(crate) fn assign_mission_state(
    unit_type: UnitType,
    strategy: ProductionStrategy,
    production_limit: u32,
    same_kind_counts: [u32; 4],
    recon_uses_mission_three: bool,
) -> u8 {
    const MISSION_WEIGHTS: [[u32; 4]; 5] = [
        [4, 4, 4, 1],
        [4, 3, 3, 1],
        [3, 3, 3, 2],
        [1, 2, 2, 4],
        [1, 1, 1, 7],
    ];

    let kind = GbUnitKind::production_order(unit_type);
    if kind < 0x04 {
        return 0;
    }
    if kind >= 0x26 || matches!(kind, 0x0A | 0x10 | 0x18 | 0x20 | 0x24) {
        return 3;
    }
    if kind == 0x16 {
        return u8::from(recon_uses_mission_three) * 3;
    }

    // 62D2〜62E4は装甲車/輸送ヘリだけ表の行を4-C6ADへ反転する。
    // 上の早期returnにより通常ROMデータでは到達しないが、分岐自体も保存する。
    let row = if matches!(kind, 0x16 | 0x24) {
        4 - strategy.index()
    } else {
        strategy.index()
    };
    let mut best_score = u32::MAX;
    let mut selected = 0_u8;
    // B=3から0へ走査し、同値では更新しないため大きい状態番号が残る。
    for state in (0_usize..=3).rev() {
        let score =
            same_kind_counts[state].saturating_add(production_limit / MISSION_WEIGHTS[row][state]);
        if score < best_score {
            best_score = score;
            selected = state as u8;
        }
    }
    selected
}

/// HRAM FFDC。設定画面から実測するとIQ100は1、IQ200は0になる。
pub(crate) fn movement_evaluation_penalty(version: AiVersion) -> u32 {
    match version {
        AiVersion::V100 => 1,
        AiVersion::V200 => 0,
        _ => unreachable!("V100/V200専用AI以外からGBのIQ補正を参照しました"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::MovementType;

    fn stats(unit_type: UnitType) -> UnitStats {
        UnitStats {
            unit_type,
            movement_type: MovementType::Tank,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        }
    }

    #[test]
    fn actor_passes_follow_bank2_call_order() {
        let supply = stats(UnitType::SupplyTruck);
        let mut indirect = stats(UnitType::Rockets);
        indirect.min_range = 2;
        let recon = stats(UnitType::Recon);
        let infantry = stats(UnitType::Infantry);
        let lander = stats(UnitType::Lander);

        assert_eq!(
            actor_selection_pass(&stats(UnitType::Carrier), false, true),
            ActorSelectionPass::MinimumObjectiveCost
        );
        assert_eq!(
            actor_selection_pass(&supply, false, true),
            ActorSelectionPass::Kind18
        );
        assert_eq!(
            actor_selection_pass(&indirect, false, true),
            ActorSelectionPass::Indirect
        );
        assert_eq!(
            actor_selection_pass(&recon, true, true),
            ActorSelectionPass::MobileOnProperty
        );
        assert_eq!(
            actor_selection_pass(&infantry, true, true),
            ActorSelectionPass::OnProperty
        );
        assert_eq!(
            actor_selection_pass(&lander, false, true),
            ActorSelectionPass::SpecialMobileKind
        );
        assert_eq!(
            actor_selection_pass(&infantry, false, false),
            ActorSelectionPass::UnassignedObjective
        );
        assert_eq!(
            actor_selection_pass(&infantry, false, true),
            ActorSelectionPass::MinimumObjectiveCost
        );
    }

    #[test]
    fn all_openwars_units_follow_the_rom_unit_table_order() {
        let kinds = [
            UnitType::Infantry,
            UnitType::Mech,
            UnitType::TankZ,
            UnitType::MdTank,
            UnitType::Tank,
            UnitType::Artillery,
            UnitType::HeavySpGun,
            UnitType::LightSpGun,
            UnitType::Missiles,
            UnitType::AntiAir,
            UnitType::Rockets,
            UnitType::Recon,
            UnitType::SupplyTruck,
            UnitType::HeavyFighter,
            UnitType::Fighter,
            UnitType::Bomber,
            UnitType::Bcopters,
            UnitType::TransportHelicopter,
            UnitType::Battleship,
            UnitType::Carrier,
            UnitType::Lander,
        ];

        assert!(
            kinds
                .windows(2)
                .all(|pair| GbUnitKind::production_order(pair[0])
                    < GbUnitKind::production_order(pair[1]))
        );
        assert_eq!(GbUnitKind::production_order(UnitType::Carrier), 0x2A);
    }

    #[test]
    fn pickup_distance_gate_matches_rom_687c() {
        assert_eq!(
            GbUnitKind::pickup_distance_threshold(UnitType::Infantry),
            12
        );
        assert_eq!(GbUnitKind::pickup_distance_threshold(UnitType::Mech), 10);
        assert_eq!(
            GbUnitKind::pickup_distance_threshold(UnitType::Artillery),
            1
        );
        assert_eq!(GbUnitKind::pickup_distance_threshold(UnitType::Rockets), 20);
        assert_eq!(GbUnitKind::pickup_distance_threshold(UnitType::Recon), 20);
        assert_eq!(GbUnitKind::pickup_distance_threshold(UnitType::Tank), 0xFF);
    }

    #[test]
    fn influence_weights_decode_the_two_rom_6894_components() {
        assert_eq!(GbUnitKind::influence_weights(UnitType::Missiles), (3, 0));
        assert_eq!(GbUnitKind::influence_weights(UnitType::Bomber), (0, 3));
        assert_eq!(GbUnitKind::influence_weights(UnitType::Battleship), (3, 3));
        assert_eq!(GbUnitKind::influence_weights(UnitType::SupplyTruck), (0, 0));
    }

    #[test]
    fn production_strategy_uses_rom_thresholds() {
        assert_eq!(production_strategy(60), ProductionStrategy::Advantage);
        assert_eq!(production_strategy(59), ProductionStrategy::Balanced);
        assert_eq!(production_strategy(40), ProductionStrategy::Balanced);
        assert_eq!(production_strategy(39), ProductionStrategy::Disadvantage);

        let mut state = RomAiState::default();
        state.set_production_strategy(PlayerId(1), ProductionStrategy::Balanced);
        assert_eq!(
            state.production_strategy_for(PlayerId(1)),
            Some(ProductionStrategy::Balanced)
        );
    }

    #[test]
    fn special_production_mode_is_rechecked_after_each_produced_unit() {
        assert_eq!(
            special_production_mode(0, 3),
            Some(SpecialProductionMode::Pickup)
        );
        // ROM 613D〜614Aは偵察車の生産後にC6A5を2減らすため、次の生産は通常走査へ戻る。
        assert_eq!(special_production_mode(0, 1), None);
        assert_eq!(
            special_production_mode(2, 3),
            Some(SpecialProductionMode::Mobility)
        );
    }

    #[test]
    fn mission_states_follow_bank2_weighted_distribution_and_tie_order() {
        assert_eq!(
            assign_mission_state(
                UnitType::Bcopters,
                ProductionStrategy::Opening,
                3,
                [0, 0, 0, 0],
                false,
            ),
            2
        );
        assert_eq!(
            assign_mission_state(
                UnitType::Bcopters,
                ProductionStrategy::Opening,
                3,
                [0, 0, 1, 0],
                false,
            ),
            1
        );
        assert_eq!(
            assign_mission_state(
                UnitType::Infantry,
                ProductionStrategy::Opening,
                11,
                [0, 0, 0, 0],
                false,
            ),
            0
        );
        assert_eq!(
            assign_mission_state(
                UnitType::TransportHelicopter,
                ProductionStrategy::Opening,
                3,
                [0, 0, 0, 0],
                false,
            ),
            3
        );
    }

    #[test]
    fn iq_levels_match_the_measured_ffdc_values() {
        assert_eq!(movement_evaluation_penalty(AiVersion::V100), 1);
        assert_eq!(movement_evaluation_penalty(AiVersion::V200), 0);
    }

    #[test]
    fn action_counters_reset_once_per_player_turn() {
        let mut state = RomAiState::default();
        let player = PlayerId(1);
        state.begin_action_turn(player, 2);
        state.record_pickup_candidate(player);
        state.record_mobility_shortage(player);
        state.begin_action_turn(player, 2);
        assert_eq!(state.production_counters(player), (1, 1));

        state.begin_action_turn(player, 3);
        assert_eq!(state.production_counters(player), (0, 0));
    }

    #[test]
    fn restricted_mode_keeps_the_rom_target_for_the_following_production_step() {
        let mut state = RomAiState::default();
        let player = PlayerId(1);
        state.set_evaluation_mode(player, RomEvaluationMode::Restricted, Some(UnitType::Recon));

        assert_eq!(
            state.evaluation_mode_for(player),
            RomEvaluationMode::Restricted
        );
        assert_eq!(state.restricted_target_for(player), Some(UnitType::Recon));

        state.set_evaluation_mode(player, RomEvaluationMode::Normal, None);
        assert_eq!(state.evaluation_mode_for(player), RomEvaluationMode::Normal);
        assert_eq!(state.restricted_target_for(player), None);
    }

    #[test]
    fn pickup_eligibility_follows_unit_record_lifetime() {
        let mut state = RomAiState::default();
        let player = PlayerId(1);
        let first = Entity::from_raw(1);
        let second = Entity::from_raw(2);

        state.observe_units(player, [first]);
        assert_eq!(state.pickup_eligible_units(player), HashSet::from([first]));

        state.set_pickup_eligible(player, first, false);
        state.observe_units(player, [first, second]);
        assert_eq!(state.pickup_eligible_units(player), HashSet::from([second]));

        state.observe_units(player, [first]);
        assert!(state.pickup_eligible_units(player).is_empty());
    }
}
