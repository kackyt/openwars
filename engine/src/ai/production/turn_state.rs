//! 当手番の生産計画状態（キャンペーン生産キュー・汎用生産予算・一括計画済みフラグ）。手番境界で破棄する。
use crate::components::PlayerId;
use bevy_ecs::prelude::*;
use std::collections::VecDeque;

/// campaign専用列から取り出した結果。取り消した案の意図解放は版別の呼び出し側が行う。
pub(crate) struct CampaignIssue {
    /// 発行直前の検証を通った命令。
    pub(crate) command: Option<crate::events::ProduceUnitCommand>,
    /// 検証に失敗し、発行せずに列から外した命令。
    pub(crate) cancelled: Vec<crate::events::ProduceUnitCommand>,
    /// 列を計画した手番。取り消した命令の発注記録はこの手番で照合する。
    pub(crate) planned_turn: u32,
}

#[derive(Resource, Default)]
pub(crate) struct ProductionTurnState {
    player_id: Option<PlayerId>,
    planned_turn: u32,
    campaign_production_planned: bool,
    campaign_production_commands: VecDeque<crate::events::ProduceUnitCommand>,
    campaign_production_blocks_generic: bool,
    campaign_production_generic_budget: Option<u32>,
    production_batch_planned: bool,
}
impl ProductionTurnState {
    /// 発行する案を取り出す。取り消しは共通の監査へ記録し、取り消した案も返す。
    pub(crate) fn take_issuable_campaign_command(
        &mut self,
        world: &mut World,
        player_id: PlayerId,
    ) -> CampaignIssue {
        let mut issue = CampaignIssue {
            command: None,
            cancelled: Vec::new(),
            planned_turn: self.planned_turn,
        };
        if self.player_id != Some(player_id) {
            return issue;
        }
        // 終局後も旧来の購入試行ログを残す。発行入口が検証し、監査だけを記録する。
        if world
            .get_resource::<crate::resources::MatchState>()
            .is_some_and(|state| state.game_over.is_some())
        {
            issue.command = self.campaign_production_commands.pop_front();
            return issue;
        }
        (issue.command, issue.cancelled) = crate::ai::production::commit::next_issuable(
            world,
            self.planned_turn,
            &mut self.campaign_production_commands,
            |command| command,
        );
        issue
    }

    #[cfg(test)]
    pub(crate) fn set_campaign_production_plan(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        commands: Vec<crate::events::ProduceUnitCommand>,
        completed_all_rows: bool,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.planned_turn = turn;
        self.campaign_production_planned = true;
        self.campaign_production_commands = VecDeque::from(commands);
        self.campaign_production_blocks_generic = !completed_all_rows;
        self.campaign_production_generic_budget = None;
    }
    pub(crate) fn set_campaign_production_plan_with_generic_budget(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        commands: Vec<crate::events::ProduceUnitCommand>,
        generic_budget: u32,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.planned_turn = turn;
        self.campaign_production_planned = true;
        self.campaign_production_commands = VecDeque::from(commands);
        self.campaign_production_blocks_generic = generic_budget == 0;
        self.campaign_production_generic_budget = Some(generic_budget);
    }
    pub(crate) fn campaign_production_planned(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.campaign_production_planned
    }
    #[cfg(test)]
    pub(crate) fn take_campaign_production_command(
        &mut self,
        player_id: PlayerId,
    ) -> Option<crate::events::ProduceUnitCommand> {
        (self.player_id == Some(player_id))
            .then(|| self.campaign_production_commands.pop_front())
            .flatten()
    }
    pub(crate) fn campaign_production_blocks_generic(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.campaign_production_blocks_generic
    }
    pub(crate) fn campaign_production_generic_budget(&self, player_id: PlayerId) -> Option<u32> {
        (self.player_id == Some(player_id))
            .then_some(self.campaign_production_generic_budget)
            .flatten()
    }
    pub(crate) fn mark_production_batch_planned(&mut self, player_id: PlayerId) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.production_batch_planned = true;
    }
    pub(crate) fn production_batch_planned(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.production_batch_planned
    }
    pub(crate) fn clear(&mut self) {
        self.player_id = None;
        self.planned_turn = 0;
        self.campaign_production_planned = false;
        self.campaign_production_commands.clear();
        self.campaign_production_blocks_generic = false;
        self.campaign_production_generic_budget = None;
        self.production_batch_planned = false;
    }
}

/// campaign専用列も汎用生産と同じ発行検証を通す。取消意図の解放は版別の呼び出し側が行う。
#[cfg(test)]
pub(crate) fn next_campaign_command(
    world: &mut World,
    player_id: PlayerId,
) -> Option<crate::events::ProduceUnitCommand> {
    let mut state = world
        .remove_resource::<ProductionTurnState>()
        .unwrap_or_default();
    let issue = state.take_issuable_campaign_command(world, player_id);
    world.insert_resource(state);
    issue.command
}
