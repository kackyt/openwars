//! 当手番で採用した戦略ポートフォリオの一時保持。手番境界で破棄する。
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

#[derive(Resource, Default)]
pub(crate) struct AdoptedTurnPortfolio {
    player_id: Option<PlayerId>,
    campaign_portfolio: Option<crate::ai::island_campaign::IslandCampaignPortfolio>,
}
impl AdoptedTurnPortfolio {
    pub(crate) fn set_campaign_portfolio(
        &mut self,
        player_id: PlayerId,
        portfolio: crate::ai::island_campaign::IslandCampaignPortfolio,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.campaign_portfolio = Some(portfolio);
    }
    pub(crate) fn campaign_portfolio(
        &self,
        player_id: PlayerId,
    ) -> Option<&crate::ai::island_campaign::IslandCampaignPortfolio> {
        (self.player_id == Some(player_id))
            .then_some(self.campaign_portfolio.as_ref())
            .flatten()
    }
    pub(crate) fn clear(&mut self) {
        self.player_id = None;
        self.campaign_portfolio = None;
    }
}
