//! 同一手番内で敵占有配置が変わらない間に再利用する行動距離キャッシュ。手番境界で破棄する。
use crate::ai::turn_distance::TurnDistanceCache;
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

#[derive(Resource, Default)]
pub(crate) struct ActionDistanceCache {
    player_id: Option<PlayerId>,
    action_distance_enemy_positions: Vec<(usize, usize)>,
    action_distance_cache: TurnDistanceCache,
    /// 敵占有マスを押し通る距離。敵配置に依存するので `action_distance_cache` と同時に破棄する。
    through_enemy_distance_cache: TurnDistanceCache,
    /// 占有を無視した地形だけの距離。敵配置が変わっても有効なので、手番内で使い回す。
    terrain_distance_cache: TurnDistanceCache,
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
            self.through_enemy_distance_cache.clear();
        }
        std::mem::take(&mut self.action_distance_cache)
    }
    pub(crate) fn put_action_distance_cache(&mut self, cache: TurnDistanceCache) {
        self.action_distance_cache = cache;
    }
    /// 敵を押し通る距離cache。`take_action_distance_cache` の後に呼ぶ（敵配置の照合はそちらで行う）。
    pub(crate) fn take_through_enemy_distance_cache(&mut self) -> TurnDistanceCache {
        std::mem::take(&mut self.through_enemy_distance_cache)
    }
    pub(crate) fn put_through_enemy_distance_cache(&mut self, cache: TurnDistanceCache) {
        self.through_enemy_distance_cache = cache;
    }
    /// 地形だけの距離cache。`take_action_distance_cache` の後に呼び、手番の勢力で取り出す。
    pub(crate) fn take_terrain_distance_cache(&mut self) -> TurnDistanceCache {
        std::mem::take(&mut self.terrain_distance_cache)
    }
    pub(crate) fn put_terrain_distance_cache(&mut self, cache: TurnDistanceCache) {
        self.terrain_distance_cache = cache;
    }
    pub(crate) fn clear(&mut self) {
        self.player_id = None;
        self.action_distance_enemy_positions.clear();
        self.action_distance_cache.clear();
        self.through_enemy_distance_cache.clear();
        self.terrain_distance_cache.clear();
    }
}
