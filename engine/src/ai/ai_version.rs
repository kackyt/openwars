use crate::components::PlayerId;
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// AIのバージョンを表すEnum。
/// 評価や意思決定アルゴリズムを新旧で切り替えるために使用します。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AiVersion {
    /// 従来のシンプルな貪欲法AI (Phase 1.5)
    V1,
    /// 部隊システム、ビーム探索、動的盤面評価(v2)を搭載した新しいAI (Phase 2)
    V2,
    /// V2 をベースに戦術評価を強化した AI (Phase 3)。
    /// - #44: 低HP時に防御地形（森・山）を優先する生存行動
    /// - #45: 間接攻撃ユニットの待ち伏せポジショニング
    /// - #49: 回復インフラ（拠点）の条件付き価値評価
    /// - #50: 敵間接攻撃の脅威マップによる露出ペナルティ
    V3,
}

impl AiVersion {
    /// V3 系の戦術評価（地形防御・待ち伏せ・脅威マップ・回復インフラ）を使うかどうか
    pub fn uses_v3_tactics(&self) -> bool {
        matches!(self, AiVersion::V3)
    }
}

/// プレイヤーごとのAI設定を管理するリソース。
/// ECSのワールドに登録し、思考時に各プレイヤーのバージョンを判定します。
#[derive(Resource, Debug, Clone, Default)]
pub struct PlayerAiSettings {
    pub versions: HashMap<PlayerId, AiVersion>,
}

impl PlayerAiSettings {
    /// 新しい PlayerAiSettings を初期化します。
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
        }
    }

    /// 指定したプレイヤーのAIバージョンを設定します。
    pub fn set_version(&mut self, player_id: PlayerId, version: AiVersion) {
        self.versions.insert(player_id, version);
    }

    /// 指定したプレイヤーのAIバージョンを取得します。デフォルトは V2 とします。
    pub fn get_version(&self, player_id: PlayerId) -> AiVersion {
        self.versions
            .get(&player_id)
            .copied()
            .unwrap_or(AiVersion::V2)
    }
}
