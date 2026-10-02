//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

impl AiVersion {
    /// 敵の有効な生産マスを占位して補充を遮断する攻勢的封鎖（Blockade）を使うかどうか。
    /// V4 限定。V3 は対戦評価・バージョン比較の基準線（ベースライン）として挙動を変更せず維持する。
    pub fn uses_enemy_production_blockade(&self) -> bool {
        matches!(self, AiVersion::V4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_inherits_v3_tactics_and_owns_operation_production() {
        // V4 は戦術評価を V3 から継承し、生産だけを作戦駆動へ差し替える。
        assert!(AiVersion::V4.uses_v3_tactics());
        assert!(AiVersion::V4.uses_operation_driven_production());
        assert!(AiVersion::V4.uses_enemy_production_blockade());
        // 過去バージョンは作戦駆動生産を使わないベースラインのまま。
        assert!(!AiVersion::V3.uses_operation_driven_production());
        assert!(!AiVersion::V3.uses_enemy_production_blockade());
        assert!(!AiVersion::V2.uses_operation_driven_production());
        assert!(!AiVersion::V1.uses_operation_driven_production());
        assert!(!AiVersion::V1.uses_v3_tactics());
    }
}
