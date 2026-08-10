//! V4（作戦駆動生産）の中核となる「作戦（Operation）」の定義と、
//! 観測量だけから 5 つの枠（占領枠・撃破枠・護衛枠・輸送枠・迎撃枠）を
//! 逆算する純粋関数群。
//!
//! 設計上の禁止事項（openspec/changes/ai-operation-driven-production/design.md）:
//! - マップ名・マップ属性による分岐を書かない
//! - 具体的なユニット名を書かない
//! - トポロジ前提の距離・隣接の仮定を置かない
//!
//! ここに現れるのは「拠点数」「敵戦力価値」「収入」「ETA」「搭載スロット」といった
//! 盤面から観測できる量だけであり、`GamePhase` による一律の理想構成は使用しない。

use crate::ai::island_campaign::combat_overmatch_requirement;

/// 展開リードタイムがこのターン数以下なら「逐次補充」で足りると判断する閾値。
pub const SHORT_LEAD_TIME_TURNS: u32 = 2;

/// 見送り購入（資金を貯めて上位ユニットを買う）の最大待機ターン数。
pub const RESERVATION_PATIENCE_TURNS: u32 = 5;

/// 1 作戦あたりの占領枠の上限。面で取る性質上大きめだが、無限には広げない。
const MAX_CAPTURE_SLOTS: u32 = 8;

/// 1 作戦あたりの輸送枠（搭載スロット）の上限。
const MAX_TRANSPORT_SLOTS: u32 = 4;

/// パッケージを何回の輸送に分けて届けることを許容するか。
/// 許容展開ターン数 = 往復ターン数 × この回数、として輸送枠を逆算する。
const ALLOWED_LIFTS: u32 = 2;

/// 作戦の種別。防衛は自軍拠点を守る作戦、占領は未保有拠点を獲りにいく作戦。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// 自軍拠点が敵の占領・撃破圏に入っている前線
    Defense,
    /// 未保有（中立・敵）拠点を面で獲りにいく前線
    Capture,
}

impl OperationKind {
    /// 同格の作戦を比較するときの優先度（小さいほど先）。
    pub fn priority_rank(&self) -> u32 {
        match self {
            OperationKind::Defense => 0,
            OperationKind::Capture => 1,
        }
    }
}

/// 調達モード。フェーズではなく「展開リードタイム」と「輸送要否」だけで決まる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionMode {
    /// 逐次補充：前線がすぐ近くにあり、1体ずつ足していけば間に合う
    Replenishment,
    /// 一括編成：前線が遠い／輸送が要るため、部隊としてまとめて揃える
    SquadPackage,
}

/// 購入判断に使う枠の種別。
///
/// 護衛枠と撃破枠はどちらも「戦闘ユニットの購入」で満たされるため、
/// 二重計上を避けて `Combat` 1 つの購入枠に統合している
/// （護衛枠は下限、撃破枠は価値ベースの要求として同時に効く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// 迎撃枠：自軍の通常戦力が到達できない位置にいる脅威への対処
    Intercept,
    /// 輸送枠：前線まで運ぶための搭載スロット
    Transport,
    /// 占領枠：拠点を面で押さえる占領可能ユニット
    Capture,
    /// 護衛枠＋撃破枠：戦闘ユニット
    Combat,
}

/// 同点時の購入優先順位（前にあるものほど先）。
pub const SLOT_PRIORITY: [SlotKind; 4] = [
    SlotKind::Intercept,
    SlotKind::Transport,
    SlotKind::Capture,
    SlotKind::Combat,
];

/// 作戦が要求する 5 つの枠。単位は種別ごとに異なる。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OperationSlots {
    /// 占領枠（体数）
    pub capture_units: u32,
    /// 護衛枠（体数・戦闘ユニット購入の下限）
    pub escort_units: u32,
    /// 撃破枠（資金）
    pub destroy_budget: u32,
    /// 輸送枠（搭載スロット数）
    pub transport_slots: u32,
    /// 迎撃枠（資金）
    pub intercept_budget: u32,
}

/// 作戦要求を導出するための観測量。ここに列挙されたものが V4 の入力すべてである。
#[derive(Debug, Clone, Copy, Default)]
pub struct OperationFacts {
    /// この作戦で獲る（守る）対象となる拠点数
    pub target_property_count: u32,
    /// 展開リードタイム内に到達できる自軍占領可能ユニット数
    pub friendly_capture_units_committed: u32,
    /// 展開リードタイム内に到達できる自軍戦力価値（HP補正済み）。
    /// 到達できない脅威への対抗要員は含まない（`friendly_intercept_value_committed` 側で数える）。
    pub friendly_combat_value_committed: u32,
    /// 到達できない脅威に対抗できる自軍戦力価値（HP補正済み）
    pub friendly_intercept_value_committed: u32,
    /// 展開リードタイム内に自軍が接敵しうる敵戦力価値（HP補正済み）
    pub enemy_combat_value: u32,
    /// 占領完了期限までに、この前線へ実際に到着できる敵生産分の予算。
    pub enemy_reinforcement_budget: u32,
    /// この前線へ投入でき、観測済みの敵へ有効な最小戦闘unitのcost。
    pub minimum_combat_unit_cost: u32,
    /// この作戦へ帰属し、支配領域の拡大を妨げる敵地上・輸送unit数。
    pub territory_control_threat_units: u32,
    /// 当該unitへ到達して有効打を与えられる既存戦闘unit数。
    pub friendly_territory_control_units: u32,
    /// 敵の支配行動を阻止するまでに許される攻撃手番数。
    pub territory_control_window_turns: u32,
    /// 通常戦力が届かない脅威へ自力到達できる最小迎撃unitのcost。
    pub minimum_intercept_unit_cost: u32,
    /// 生産施設からこの作戦の代表地点までの展開リードタイム（ターン）
    pub deploy_lead_time: u32,
    /// 敵がこの作戦地点へ到達するまでのターン数
    pub enemy_contact_eta: u32,
    /// 自力では到達できず輸送が必須かどうか
    pub requires_transport: bool,
    /// 輸送1往復にかかるターン数
    pub transport_round_trip_turns: u32,
    /// 既に使える空き搭載スロット数
    pub available_free_cargo_slots: u32,
    /// 自軍の通常戦力が到達できない位置にいる脅威の価値（HP補正済み）
    pub unreachable_threat_value: u32,
}

/// 観測量から調達モードを決める。フェーズではなく展開リードタイムと輸送要否だけで決まる。
pub fn acquisition_mode(facts: &OperationFacts) -> AcquisitionMode {
    if facts.requires_transport || facts.deploy_lead_time > SHORT_LEAD_TIME_TURNS {
        AcquisitionMode::SquadPackage
    } else {
        AcquisitionMode::Replenishment
    }
}

/// 観測量から 5 枠を逆算する。
///
/// 依存順は 占領枠 → 撃破枠 → 護衛枠 → 輸送枠 → 迎撃枠。
/// 輸送枠は前段で決まった搭載対象数から導出されるため最後段に近い位置にある。
pub fn derive_slots(facts: &OperationFacts) -> OperationSlots {
    // --- 占領枠：拠点を面で押さえるため、対象拠点数から既に向かっている占領ユニットを引く ---
    let capture_units = facts
        .target_property_count
        .saturating_sub(facts.friendly_capture_units_committed)
        .min(MAX_CAPTURE_SLOTS);

    // --- 撃破枠：現在の局地敵戦力 + 占領完了前に到着できる敵増援 ---
    let projected_threat = facts
        .enemy_combat_value
        .saturating_add(facts.enemy_reinforcement_budget);
    let required_overmatch =
        combat_overmatch_requirement(projected_threat, facts.minimum_combat_unit_cost);
    // 撃破枠は「これから買い足すべき資金量」なので、既に前線へ張り付いている
    // 自軍戦力価値を差し引く。ここを引かないと毎ターン満額の要求が立ち続ける。
    let destroy_budget = required_overmatch.saturating_sub(facts.friendly_combat_value_committed);

    // 作戦地点へ張り付けるべき占領要員の総数（既に手元にいる分＋これから買う分）
    let capture_presence = capture_units.saturating_add(facts.friendly_capture_units_committed);

    // --- 護衛枠：接敵ETAが展開リードタイムより遅ければ、護衛は不要（ゼロになりうる） ---
    let base_escort_units = if facts.enemy_contact_eta > facts.deploy_lead_time {
        0
    } else {
        // 面で取る占領部隊に対し、半数を目安に護衛を付ける
        capture_presence.div_ceil(2)
    };
    // --- 拡張阻止sortie枠：価格ではなく1手番1攻撃の処理能力で数える ---
    // 高価な航空機1機をcost分の歩兵へ即時対応できると見なすと、敵が複数島へ
    // 占領兵を送り続ける局面でも追加生産が止まる。阻止期限までに1機が実行できる
    // 攻撃回数を上限とし、敵拡張unit数を処理できる実体数の不足を求める。
    let required_denial_units = facts
        .territory_control_threat_units
        .div_ceil(facts.territory_control_window_turns.max(1));
    let denial_shortage =
        required_denial_units.saturating_sub(facts.friendly_territory_control_units);
    let escort_units = base_escort_units.max(denial_shortage);

    // --- 輸送枠：占領＋護衛を運ぶのに必要な搭載スロット ---
    let transport_slots = if !facts.requires_transport {
        0
    } else {
        // 運ぶ対象は「これから買う分」ではなく「現地へ渡すべき部隊の総量」である。
        // 買う分だけで数えると、手元に占領要員が溜まった時点で占領枠が 0 に潰れ、
        // 連動して輸送要求まで消えて second front が永久に開かなくなる。
        // 一度に渡すのは 1 作戦分の波までとし、際限なく輸送を積み増さないよう頭打ちにする。
        let payload = capture_presence
            .min(MAX_CAPTURE_SLOTS)
            .saturating_add(escort_units);
        payload
            .div_ceil(ALLOWED_LIFTS.max(1))
            .saturating_sub(facts.available_free_cargo_slots)
            .min(MAX_TRANSPORT_SLOTS)
    };

    // --- 迎撃枠：通常戦力が到達できない位置の脅威だけを対象にする ---
    // 「制空で応じるか対空で応じるか」は思想ではなく到達可能性の問題なので、
    // 到達できない脅威をここに切り出し、到達できる候補だけで満たす。
    // 撃破枠と同様に、既に保有している対抗要員の価値を差し引く。
    // これを引かないと脅威が減らない限り毎ターン同じ要求が満額で立ち続け、
    // 対空ユニットを買い増し続けるラチェットになる。
    let gross_intercept = combat_overmatch_requirement(
        facts.unreachable_threat_value,
        facts.minimum_intercept_unit_cost,
    );
    let intercept_budget = gross_intercept.saturating_sub(facts.friendly_intercept_value_committed);

    OperationSlots {
        capture_units,
        escort_units,
        destroy_budget,
        transport_slots,
        intercept_budget,
    }
}

/// 枠の要求の性質。購入順を決めるときにこの 2 つを混ぜてはならない。
///
/// 未充足率は要求量で正規化されるため、要求が青天井の枠は買っても買っても
/// 1.0 から下がらない。有限要求の枠と同じ土俵で「最も飢えた枠」を選ぶと、
/// 青天井の枠が恒久的に勝ち、前提条件が永久に揃わなくなる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotTier {
    /// 前提条件：要求量が敵情・地形から有限に決まる（迎撃・輸送・占領・護衛）
    Prerequisite,
    /// 余剰の注ぎ先：要求量が「投入できる資金」そのもので青天井（撃破）
    Residual,
}

impl OperationSlots {
    /// 段階を指定した未充足率。段階に属さない枠は 0.0（＝選択対象外）を返す。
    pub fn tier_deficit(&self, kind: SlotKind, filled: &OperationSlots, tier: SlotTier) -> f32 {
        let ratio = |required: u32, done: u32| -> f32 {
            if required == 0 {
                0.0
            } else {
                required.saturating_sub(done) as f32 / required as f32
            }
        };
        match (kind, tier) {
            // 戦闘枠だけは 2 つの要求を持つ。護衛（体数）は有限なので前提条件、
            // 撃破（資金）は青天井なので余剰、と段階を分けて評価する。
            (SlotKind::Combat, SlotTier::Prerequisite) => {
                ratio(self.escort_units, filled.escort_units)
            }
            (SlotKind::Combat, SlotTier::Residual) => {
                ratio(self.destroy_budget, filled.destroy_budget)
            }
            (_, SlotTier::Residual) => 0.0,
            (kind, SlotTier::Prerequisite) => self.deficit_ratio(kind, filled),
        }
    }

    /// 指定した購入枠の要求量を返す。単位は枠種別ごとに異なる（体数 / スロット / 資金）。
    pub fn requirement(&self, kind: SlotKind) -> u32 {
        match kind {
            SlotKind::Intercept => self.intercept_budget,
            SlotKind::Transport => self.transport_slots,
            SlotKind::Capture => self.capture_units,
            // 戦闘枠は「護衛の下限体数」と「撃破の資金要求」の両方を持つため、
            // 充足率の比較では requirement 単独では表せない（`deficit_ratio` を使う）。
            SlotKind::Combat => self.destroy_budget,
        }
    }

    /// 充足済み量 `filled` に対する未充足率 (0.0..=1.0) を返す。要求が無い枠は 0.0。
    pub fn deficit_ratio(&self, kind: SlotKind, filled: &OperationSlots) -> f32 {
        let ratio = |required: u32, done: u32| -> f32 {
            if required == 0 {
                0.0
            } else {
                required.saturating_sub(done) as f32 / required as f32
            }
        };
        match kind {
            SlotKind::Intercept => ratio(self.intercept_budget, filled.intercept_budget),
            SlotKind::Transport => ratio(self.transport_slots, filled.transport_slots),
            SlotKind::Capture => ratio(self.capture_units, filled.capture_units),
            // 護衛（体数）と撃破（資金）のうち、より不足している方を戦闘枠の不足率とする
            SlotKind::Combat => ratio(self.escort_units, filled.escort_units)
                .max(ratio(self.destroy_budget, filled.destroy_budget)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts() -> OperationFacts {
        OperationFacts {
            target_property_count: 4,
            minimum_combat_unit_cost: 1000,
            minimum_intercept_unit_cost: 1000,
            deploy_lead_time: 2,
            enemy_contact_eta: 2,
            transport_round_trip_turns: 4,
            ..Default::default()
        }
    }

    /// 占領枠は対象拠点数から既に向かっている占領ユニット数を引いた値になる
    #[test]
    fn capture_slots_are_area_based_and_net_of_committed_units() {
        let mut facts = base_facts();
        assert_eq!(derive_slots(&facts).capture_units, 4);

        facts.friendly_capture_units_committed = 3;
        assert_eq!(derive_slots(&facts).capture_units, 1);

        facts.friendly_capture_units_committed = 9;
        assert_eq!(derive_slots(&facts).capture_units, 0);
    }

    /// 期限内に到着できる敵増援だけを局地脅威へ加える
    #[test]
    fn destroy_budget_accounts_for_enemy_reinforcement() {
        let mut facts = base_facts();
        facts.enemy_combat_value = 0;
        facts.enemy_reinforcement_budget = 2000;

        assert_eq!(derive_slots(&facts).destroy_budget, 3000);
    }

    /// 局地優越を満たした後は、余剰資金を理由に戦闘要求を増やさない。
    #[test]
    fn destroy_budget_stops_after_local_overmatch_is_filled() {
        let mut facts = base_facts();
        facts.enemy_combat_value = 5000;
        facts.friendly_combat_value_committed = 6000;

        assert_eq!(derive_slots(&facts).destroy_budget, 0);
    }

    /// 資金量が局地脅威を上回っても要求量には影響しない。
    #[test]
    fn destroy_budget_is_independent_of_available_funds() {
        let mut facts = base_facts();
        facts.enemy_combat_value = 5000;

        assert_eq!(derive_slots(&facts).destroy_budget, 6000);
    }

    /// 敵が戦力も生産手段も失っていれば、撃破枠は 0 に戻る
    #[test]
    fn destroy_budget_returns_to_zero_when_enemy_is_spent() {
        let mut facts = base_facts();
        facts.enemy_combat_value = 0;

        assert_eq!(derive_slots(&facts).destroy_budget, 0);
    }

    /// 自軍の展開済み戦力は撃破枠から差し引かれる
    #[test]
    fn destroy_budget_subtracts_committed_friendly_value() {
        let mut facts = base_facts();
        facts.enemy_combat_value = 5000;
        facts.friendly_combat_value_committed = 4000;
        // 敵5,000 + 最小戦闘unit 1,000 - 配備済み4,000 = 2,000
        assert_eq!(derive_slots(&facts).destroy_budget, 2000);
    }

    /// 接敵ETAが展開リードタイムより遅ければ護衛枠はゼロになる
    #[test]
    fn escort_slots_vanish_when_contact_is_late() {
        let mut facts = base_facts();
        facts.deploy_lead_time = 2;
        facts.enemy_contact_eta = 5;
        assert_eq!(derive_slots(&facts).escort_units, 0);

        facts.enemy_contact_eta = 2;
        // 占領4体に対して護衛2体
        assert_eq!(derive_slots(&facts).escort_units, 2);
    }

    #[test]
    fn territory_control_uses_attack_bodies_instead_of_unit_price() {
        let mut facts = base_facts();
        facts.enemy_contact_eta = 9;
        facts.deploy_lead_time = 2;
        facts.territory_control_threat_units = 5;
        facts.friendly_territory_control_units = 1;
        facts.territory_control_window_turns = 2;

        // 5目標を2手番で阻止するには3機必要で、既存1機を引いた2機が不足する。
        // unit価格やenemy_combat_valueには依存しない。
        assert_eq!(derive_slots(&facts).escort_units, 2);
    }

    /// 輸送が不要な前線では輸送枠は立たない
    #[test]
    fn transport_slots_only_when_transport_required() {
        let mut facts = base_facts();
        facts.requires_transport = false;
        assert_eq!(derive_slots(&facts).transport_slots, 0);

        facts.requires_transport = true;
        // 占領4 + 護衛2 = 6 を 2 回の輸送に分ける → 3 スロット
        assert_eq!(derive_slots(&facts).transport_slots, 3);

        // 既存の空きスロットは差し引かれる
        facts.available_free_cargo_slots = 2;
        assert_eq!(derive_slots(&facts).transport_slots, 1);
    }

    /// 調達モードは展開リードタイムと輸送要否だけで決まる
    #[test]
    fn acquisition_mode_depends_on_lead_time_and_transport() {
        let mut facts = base_facts();
        facts.deploy_lead_time = 1;
        facts.requires_transport = false;
        assert_eq!(acquisition_mode(&facts), AcquisitionMode::Replenishment);

        facts.deploy_lead_time = 5;
        assert_eq!(acquisition_mode(&facts), AcquisitionMode::SquadPackage);

        facts.deploy_lead_time = 1;
        facts.requires_transport = true;
        assert_eq!(acquisition_mode(&facts), AcquisitionMode::SquadPackage);
    }

    /// 到達できない脅威だけが迎撃枠になる
    #[test]
    fn intercept_budget_covers_unreachable_threats_only() {
        let mut facts = base_facts();
        assert_eq!(derive_slots(&facts).intercept_budget, 0);

        facts.unreachable_threat_value = 5000;
        assert_eq!(derive_slots(&facts).intercept_budget, 6000);
    }

    /// 既に保有している迎撃要員は迎撃枠から差し引かれる。
    /// これを引かないと脅威が減らない限り毎ターン満額の要求が立ち続け、
    /// 対空ユニットを買い増し続けるラチェットになる。
    #[test]
    fn intercept_budget_subtracts_committed_interceptors() {
        let mut facts = base_facts();
        facts.unreachable_threat_value = 5000;

        facts.friendly_intercept_value_committed = 2000;
        assert_eq!(derive_slots(&facts).intercept_budget, 4000);

        // 要求を満たすだけ揃っていれば追加調達は不要になる
        facts.friendly_intercept_value_committed = 6000;
        assert_eq!(derive_slots(&facts).intercept_budget, 0);
    }

    /// 占領要員が手元に揃っていても、渡せていない限り輸送要求は消えない。
    /// 「これから買う分」だけで輸送量を数えると、占領枠が充足した瞬間に
    /// 輸送枠まで 0 に潰れ、海の向こうの前線が永久に開かなくなる。
    #[test]
    fn transport_slots_survive_when_capture_units_are_already_on_hand() {
        let mut facts = base_facts();
        facts.requires_transport = true;
        facts.friendly_capture_units_committed = 4;

        // 占領枠は充足（4 - 4 = 0）だが、その 4 体を運ぶ輸送は依然必要
        let slots = derive_slots(&facts);
        assert_eq!(slots.capture_units, 0);
        assert_eq!(slots.transport_slots, 3);
    }

    /// 輸送量は 1 作戦分の波で頭打ちにし、占領要員の滞留に比例して膨張させない
    #[test]
    fn transport_slots_are_capped_at_one_wave() {
        let mut facts = base_facts();
        facts.requires_transport = true;
        facts.friendly_capture_units_committed = 40;

        // 占領 min(40, 8) = 8 + 護衛 20 = 28 を 2 回に分ける → 14 だが上限 4
        assert_eq!(derive_slots(&facts).transport_slots, MAX_TRANSPORT_SLOTS);
    }

    /// 戦闘枠の不足率は護衛（体数）と撃破（資金）の悪い方で決まる
    #[test]
    fn combat_deficit_takes_the_worse_of_escort_and_destroy() {
        let slots = OperationSlots {
            escort_units: 2,
            destroy_budget: 10000,
            ..Default::default()
        };
        let filled = OperationSlots {
            escort_units: 2,
            destroy_budget: 2000,
            ..Default::default()
        };
        // 護衛は充足済みだが撃破が 80% 不足
        assert!((slots.deficit_ratio(SlotKind::Combat, &filled) - 0.8).abs() < 1e-6);
    }
}
