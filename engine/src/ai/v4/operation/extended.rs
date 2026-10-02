//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

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
    // 将来の敵生産上限は「今この手番に倒す敵」ではない。観測済みの敵だけで
    // Combat計画を起動し、敵ユニットが実際に現れた次手番に改めて見積もる。
    let combat_plan_required = u32::from(facts.enemy_combat_units > 0);

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

    /// 未観測の敵増援余力は、現在Combatの必須要求へ加えない
    #[test]
    fn combat_plan_required_ignores_unobserved_enemy_reinforcement_capacity() {
        let mut facts = base_facts();
        facts.enemy_combat_units = 0;
        facts.enemy_reinforcement_funds = 2000;

        assert_eq!(derive_slots(&facts).combat_plan_required, 0);
    }
}
