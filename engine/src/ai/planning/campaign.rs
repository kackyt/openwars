//! campaign提案: 盤面から島嶼キャンペーンのportfolio（防衛・攻勢と兵站の要求）を作る。
//!
//! 採用（小規模V4のRoadmap選択など）はここでは行わない。部隊再編と生産判断の両方が
//! この提案を起点にする。
//!
//! 現状はWorldへ次の2つを書く（提案を純関数にするのは所属正本の一本化と合わせて扱う）。
//! - 拡張経路のV4: 首都攻略DAGの準備（`v4::strategy_pipeline::prepare_strategy`）
//! - 診断用の [`IslandCampaignDiagnostics`]（意思決定には戻さない）

use crate::ai::island_campaign::{IslandCampaignDiagnostics, IslandCampaignPortfolio};
use crate::ai::island_campaign_analysis::{
    analyze_island_campaign, analyze_island_campaign_excluding,
};
use crate::collections::HashSet;
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

/// 盤面からcampaign portfolioを提案する（V3以降）。
///
/// `reserved_entities` は既存の実行任務へ予約済みで、portfolioへ割り当てないEntity。
pub(crate) fn propose(
    world: &mut World,
    player_id: PlayerId,
    reserved_entities: &HashSet<Entity>,
) -> IslandCampaignPortfolio {
    let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
    // 首都攻略DAGの準備hookは拡張経路のV4だけが使う。
    let prepares_strategy_pipeline =
        ai_version.uses_operation_driven_production() && crate::ai::code_path::is_extended();
    let mut portfolio = if reserved_entities.is_empty() {
        analyze_island_campaign(world, player_id)
    } else {
        analyze_island_campaign_excluding(world, player_id, reserved_entities)
    };
    if prepares_strategy_pipeline {
        // 小規模/通常の分岐はここで持たず、ターン入口で確定したパイプラインへ
        // portfolioを渡す。各層が別々に小規模作戦を有効化しないための境界である。
        crate::ai::v4::strategy_pipeline::prepare_strategy(world, player_id, &mut portfolio);
    }
    // 診断Resourceは意思決定に戻さず、最後の分析結果だけをプレイヤー別に上書きする。
    world
        .get_resource_or_insert_with(IslandCampaignDiagnostics::default)
        .by_player
        .insert(player_id, portfolio.clone());
    portfolio
}
