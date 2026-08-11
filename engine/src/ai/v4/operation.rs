//! V4（作戦駆動生産）の中核となる「作戦（Operation）」の定義と、
//! 観測量だけから4つの枠（占領枠・戦闘計画・輸送枠・迎撃枠）を
//! 逆算する純粋関数群。
//!
//! 設計上の禁止事項（openspec/changes/ai-operation-driven-production/design.md）:
//! - マップ名・マップ属性による分岐を書かない
//! - 具体的なユニット名を書かない
//! - トポロジ前提の距離・隣接の仮定を置かない
//!
//! ここに現れるのは「拠点数」「敵Entity」「収入」「ETA」「搭載スロット」といった
//! 盤面から観測できる量だけであり、`GamePhase` による一律の理想構成は使用しない。

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
    /// 勝利条件へ接続する敵首都攻略。兵站作戦と並行して戦力を形成する。
    AssaultCapital,
}

impl OperationKind {
    /// 同格の作戦を比較するときの優先度（小さいほど先）。
    pub fn priority_rank(&self) -> u32 {
        match self {
            OperationKind::Defense => 0,
            OperationKind::Capture => 1,
            OperationKind::AssaultCapital => 2,
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
/// Combatは購入数や価格の枠ではなく、具体的な手番列を探索するRollingPlanの起動要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// 迎撃枠：自軍の通常戦力が到達できない位置にいる脅威への対処
    Intercept,
    /// 輸送枠：前線まで運ぶための搭載スロット
    Transport,
    /// 占領枠：拠点を面で押さえる占領可能ユニット
    Capture,
    /// 戦闘計画：対象Entityの排除schedule
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
    /// 観測敵Entityに対するターン別戦闘計画を起動する要求（0または1）。
    pub combat_plan_required: u32,
    /// 輸送枠（搭載スロット数）
    pub transport_slots: u32,
    /// 通常戦力が届かない敵Entityに対する迎撃Entity数。
    pub intercept_units: u32,
}

/// 作戦要求を導出するための観測量。ここに列挙されたものが V4 の入力すべてである。
#[derive(Debug, Clone, Copy, Default)]
pub struct OperationFacts {
    /// この作戦で獲る（守る）対象となる拠点数
    pub target_property_count: u32,
    /// 展開リードタイム内に到達できる自軍占領可能ユニット数
    pub friendly_capture_units_committed: u32,
    /// 展開リードタイム内に自軍が接敵しうる敵Entity数。
    pub enemy_combat_units: u32,
    /// 同じ作戦へ実際に参加できる既存戦闘Entity数。診断用で、必要編成は計画器が決める。
    pub friendly_combat_units_committed: u32,
    /// 敵施設が期限内に増援を生産できる資金。増援構成は別途ターン別に展開する。
    pub enemy_reinforcement_funds: u32,
    /// 到達不能脅威へ対抗できる既存迎撃Entity数。
    pub friendly_intercept_units_committed: u32,
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
    /// 自軍の通常戦力が到達できない位置にいる敵Entity数。
    pub unreachable_threat_units: u32,
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
/// 依存順は 占領枠 → 戦闘計画 → 輸送枠 → 迎撃枠。
/// 輸送枠は前段で決まった搭載対象数から導出されるため最後段に近い位置にある。
pub fn derive_slots(facts: &OperationFacts) -> OperationSlots {
    // --- 占領枠：拠点を面で押さえるため、対象拠点数から既に向かっている占領ユニットを引く ---
    let capture_units = facts
        .target_property_count
        .saturating_sub(facts.friendly_capture_units_committed)
        .min(MAX_CAPTURE_SLOTS);

    // --- 戦闘計画：対象Entityがあれば必要編成をrolling plannerへ委譲する ---
    let combat_plan_required =
        u32::from(facts.enemy_combat_units > 0 || facts.enemy_reinforcement_funds > 0);

    // 作戦地点へ届ける占領要員の総数（既に手元にいる分＋これから買う分）
    let capture_presence = capture_units.saturating_add(facts.friendly_capture_units_committed);

    // --- 輸送枠：占領要員を運ぶのに必要な搭載スロット ---
    let transport_slots = if !facts.requires_transport {
        0
    } else {
        // 運ぶ対象は「これから買う分」ではなく「現地へ渡すべき部隊の総量」である。
        // 買う分だけで数えると、手元に占領要員が溜まった時点で占領枠が 0 に潰れ、
        // 連動して輸送要求まで消えて second front が永久に開かなくなる。
        // 一度に渡すのは 1 作戦分の波までとし、際限なく輸送を積み増さないよう頭打ちにする。
        let payload = capture_presence.min(MAX_CAPTURE_SLOTS);
        payload
            .div_ceil(ALLOWED_LIFTS.max(1))
            .saturating_sub(facts.available_free_cargo_slots)
            .min(MAX_TRANSPORT_SLOTS)
    };

    // --- 迎撃枠：実在する到達不能敵と既存迎撃Entityの個数差 ---
    let intercept_units = facts
        .unreachable_threat_units
        .saturating_sub(facts.friendly_intercept_units_committed);

    OperationSlots {
        capture_units,
        combat_plan_required,
        transport_slots,
        intercept_units,
    }
}

/// 枠の要求の性質。旧Residual価格要求は廃止済みで、全要求が有限である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotTier {
    /// 前提条件：要求量が敵情・地形から有限に決まる。
    Prerequisite,
    /// 互換用。価格ベースの余剰Combat要求は存在しない。
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
            (SlotKind::Combat, SlotTier::Prerequisite) => {
                ratio(self.combat_plan_required, filled.combat_plan_required)
            }
            (SlotKind::Combat, SlotTier::Residual) => 0.0,
            (_, SlotTier::Residual) => 0.0,
            (kind, SlotTier::Prerequisite) => self.deficit_ratio(kind, filled),
        }
    }

    /// 指定した購入枠の要求量を返す。単位は体数・スロット・計画起動フラグ。
    pub fn requirement(&self, kind: SlotKind) -> u32 {
        match kind {
            SlotKind::Intercept => self.intercept_units,
            SlotKind::Transport => self.transport_slots,
            SlotKind::Capture => self.capture_units,
            SlotKind::Combat => self.combat_plan_required,
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
            SlotKind::Intercept => ratio(self.intercept_units, filled.intercept_units),
            SlotKind::Transport => ratio(self.transport_slots, filled.transport_slots),
            SlotKind::Capture => ratio(self.capture_units, filled.capture_units),
            SlotKind::Combat => ratio(self.combat_plan_required, filled.combat_plan_required),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts() -> OperationFacts {
        OperationFacts {
            target_property_count: 4,
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
    fn combat_plan_required_accounts_for_enemy_reinforcement() {
        let mut facts = base_facts();
        facts.enemy_combat_units = 0;
        facts.enemy_reinforcement_funds = 2000;

        assert_eq!(derive_slots(&facts).combat_plan_required, 1);
    }

    /// 既存戦力がいても、対象Entityを全滅できるかは計画器で再評価する。
    #[test]
    fn combat_plan_required_is_not_suppressed_by_friendly_unit_count() {
        let mut facts = base_facts();
        facts.enemy_combat_units = 5;
        facts.friendly_combat_units_committed = 6;

        assert_eq!(derive_slots(&facts).combat_plan_required, 1);
    }

    /// 資金量が局地脅威を上回っても要求量には影響しない。
    #[test]
    fn combat_plan_required_is_independent_of_available_funds() {
        let mut facts = base_facts();
        facts.enemy_combat_units = 5;

        assert_eq!(derive_slots(&facts).combat_plan_required, 1);
    }

    /// 敵が戦力も生産手段も失っていれば、撃破枠は 0 に戻る
    #[test]
    fn combat_plan_required_returns_to_zero_when_enemy_is_spent() {
        let mut facts = base_facts();
        facts.enemy_combat_units = 0;

        assert_eq!(derive_slots(&facts).combat_plan_required, 0);
    }

    /// 輸送が不要な前線では輸送枠は立たない
    #[test]
    fn transport_slots_only_when_transport_required() {
        let mut facts = base_facts();
        facts.requires_transport = false;
        assert_eq!(derive_slots(&facts).transport_slots, 0);

        facts.requires_transport = true;
        // 占領4体を2回の輸送に分ける → 2スロット
        assert_eq!(derive_slots(&facts).transport_slots, 2);

        // 既存の空きスロットは差し引かれる
        facts.available_free_cargo_slots = 2;
        assert_eq!(derive_slots(&facts).transport_slots, 0);
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
    fn intercept_units_covers_unreachable_threats_only() {
        let mut facts = base_facts();
        assert_eq!(derive_slots(&facts).intercept_units, 0);

        facts.unreachable_threat_units = 5;
        assert_eq!(derive_slots(&facts).intercept_units, 5);
    }

    /// 既に保有している迎撃要員は迎撃枠から差し引かれる。
    /// これを引かないと脅威が減らない限り毎ターン満額の要求が立ち続け、
    /// 対空ユニットを買い増し続けるラチェットになる。
    #[test]
    fn intercept_units_subtracts_committed_interceptors() {
        let mut facts = base_facts();
        facts.unreachable_threat_units = 5;

        facts.friendly_intercept_units_committed = 2;
        assert_eq!(derive_slots(&facts).intercept_units, 3);

        // 要求を満たすだけ揃っていれば追加調達は不要になる
        facts.friendly_intercept_units_committed = 6;
        assert_eq!(derive_slots(&facts).intercept_units, 0);
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
        assert_eq!(slots.transport_slots, 2);
    }

    /// 輸送量は 1 作戦分の波で頭打ちにし、占領要員の滞留に比例して膨張させない
    #[test]
    fn transport_slots_are_capped_at_one_wave() {
        let mut facts = base_facts();
        facts.requires_transport = true;
        facts.friendly_capture_units_committed = 40;

        // 占領 min(40, 8) = 8 を2回に分ける → 上限と同じ4
        assert_eq!(derive_slots(&facts).transport_slots, MAX_TRANSPORT_SLOTS);
    }

    /// 戦闘計画の起動要求は0/1であり、価格の比率へ変換しない。
    #[test]
    fn combat_deficit_is_a_binary_plan_trigger() {
        let slots = OperationSlots {
            combat_plan_required: 1,
            ..Default::default()
        };
        assert_eq!(
            slots.deficit_ratio(SlotKind::Combat, &Default::default()),
            1.0
        );
        let filled = OperationSlots {
            combat_plan_required: 1,
            ..Default::default()
        };
        assert_eq!(slots.deficit_ratio(SlotKind::Combat, &filled), 0.0);
    }
}
