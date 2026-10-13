//! AI再設計: 責務ごとに分離した共通処理。
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_min_or_max)]
#![allow(clippy::unnecessary_map_or)]
use super::production_queue::AiProductionCooldown;
use crate::collections::HashSet;
use bevy_ecs::prelude::*;

#[derive(Resource, Default)]
pub struct AiActionCooldown(pub HashSet<Entity>);

/// ターン開始時にAIの冷却リストをクリアするシステム。
pub fn clear_ai_cooldowns_system(
    mut events: EventReader<crate::events::GamePhaseChangedEvent>,
    action_cooldown: Option<ResMut<AiActionCooldown>>,
    prod_cooldown: Option<ResMut<AiProductionCooldown>>,
) {
    if events.is_empty() {
        return;
    }
    events.clear();

    if let Some(mut ac) = action_cooldown {
        ac.0.clear();
    }
    if let Some(mut pc) = prod_cooldown {
        pc.0.clear();
    }
}

use crate::components::PlayerId;

#[derive(Resource, Default)]
pub(crate) struct AiTurnProgress {
    player_id: Option<PlayerId>,
    squads_planned: bool,
}
impl AiTurnProgress {
    pub(crate) fn mark_squads_planned(&mut self, player_id: PlayerId) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.squads_planned = true;
    }
    pub(crate) fn squads_planned(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.squads_planned
    }
    pub(crate) fn clear(&mut self) {
        self.player_id = None;
        self.squads_planned = false;
    }
}
