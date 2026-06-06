use bevy_ecs::prelude::*;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u32);

impl PlayerId {
    /// 2プレイヤーゲームにおいて、もう一方のプレイヤーのIDを返します。
    pub fn opposite(&self) -> Self {
        Self(if self.0 == 1 { 2 } else { 1 })
    }
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faction(pub PlayerId);
