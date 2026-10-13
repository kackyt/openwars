//! 手番境界で破棄する、所有者ごとの一時状態。
use crate::ai::turn_distance::TurnDistanceCache;
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

#[derive(Resource, Default)]
pub(crate) struct ActionDistanceCache {
    player_id: Option<PlayerId>,
    action_distance_enemy_positions: Vec<(usize, usize)>,
    action_distance_cache: TurnDistanceCache,
}
impl ActionDistanceCache {
    pub(crate) fn take_action_distance_cache(
        &mut self,
        player_id: PlayerId,
        enemy_positions: &[(usize, usize)],
    ) -> TurnDistanceCache {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        if self.action_distance_enemy_positions != enemy_positions {
            self.action_distance_enemy_positions = enemy_positions.to_vec();
            self.action_distance_cache.clear();
        }
        std::mem::take(&mut self.action_distance_cache)
    }
    pub(crate) fn put_action_distance_cache(&mut self, cache: TurnDistanceCache) {
        self.action_distance_cache = cache;
    }
    pub(crate) fn clear(&mut self) {
        self.player_id = None;
        self.action_distance_enemy_positions.clear();
        self.action_distance_cache.clear();
    }
}
