//! 購入案の確定と発行直前の検証。
//!
//! 購入は次の状態を区別する。
//! - Planned: 当手番の配分で確定し、発行待ちの列へ入った購入案。
//! - Issued: 生産命令として発行した（ゲームSystemが適用するまで成功ではない）。
//! - ObservedSuccess / Failed: 生産完了イベントとの照合結果（照合は版別Registryが行う）。
//!
//! 計画を立てた時点から発行までの間に資金・施設の状態は変わり得るため、
//! 発行直前に `validate_issue` で実盤面に照らして再検証する。検証に失敗した購入案は
//! 発行せず（未発行の取消）、理由を `IssueAudit` に残す。

use crate::components::UnitStats;
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Players, UnitRegistry};
use bevy_ecs::prelude::*;

/// 購入の目的。継続計画の工程だけが工程参照を必須にし、形式だけの計画は作らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurchasePurpose<StepRef> {
    /// 永続計画の工程を実行する購入。
    PlanStep(StepRef),
    /// 永続計画を持たない作戦枠（占領・輸送・迎撃・計画化されない戦闘）を埋める購入。
    OperationSlot,
    /// 実在敵への即時増援として、残額から追加する購入。
    ImmediateReinforcement,
    /// 空き施設と余剰資金で前線作戦へ送る動員購入。
    Mobilization,
}

/// 発行直前の検証で購入案を発行しない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueRejection {
    /// 命令のプレイヤーが存在しない。
    UnknownPlayer,
    /// 兵種の能力値が登録されていない。
    UnknownUnitType,
    /// 計画後に資金が減り、生産費を払えない。
    InsufficientFunds { required: u32, available: u32 },
    /// 施設の喪失・占有・生産不可など、ゲームの生産規則を満たさない。
    FacilityUnavailable(String),
}

/// 発行しなかった購入案の記録（診断用、判断には使わない）。
#[derive(Debug, Clone)]
pub struct RejectedIssue {
    pub turn: u32,
    pub command: ProduceUnitCommand,
    pub rejection: IssueRejection,
}

/// 発行直前の検証で取り消した購入案の監査記録。
#[derive(Resource, Debug, Default)]
pub struct IssueAudit {
    rejected: Vec<RejectedIssue>,
}

impl IssueAudit {
    /// 保持する件数の上限。診断用なので古い記録から捨てる。
    const CAPACITY: usize = 256;

    /// 取り消しを記録する。
    pub fn record(&mut self, turn: u32, command: ProduceUnitCommand, rejection: IssueRejection) {
        if self.rejected.len() >= Self::CAPACITY {
            self.rejected.remove(0);
        }
        self.rejected.push(RejectedIssue {
            turn,
            command,
            rejection,
        });
    }

    /// 記録済みの取り消し。
    pub fn rejected(&self) -> &[RejectedIssue] {
        &self.rejected
    }
}

/// 生産命令を、発行直前の実盤面（資金・施設）に照らして検証する。
///
/// ゲームSystemの生産規則（`can_produce_at`）と資金条件をそのまま使う。
/// 検証に必要なリソースが無いテスト用Worldでは、その項目の検証を省略する。
pub fn validate_issue(
    world: &mut World,
    command: &ProduceUnitCommand,
) -> Result<(), IssueRejection> {
    if let Some(players) = world.get_resource::<Players>() {
        let Some(player) = players.0.iter().find(|p| p.id == command.player_id) else {
            return Err(IssueRejection::UnknownPlayer);
        };
        let available = player.funds;
        if let Some(registry) = world.get_resource::<UnitRegistry>() {
            let Some(required) = registry
                .get_stats(command.unit_type)
                .map(|stats: &UnitStats| stats.cost)
            else {
                return Err(IssueRejection::UnknownUnitType);
            };
            if available < required {
                return Err(IssueRejection::InsufficientFunds {
                    required,
                    available,
                });
            }
        }
    }
    if world.contains_resource::<MasterDataRegistry>() {
        world
            .resource_scope(|world, master: Mut<MasterDataRegistry>| {
                crate::systems::production::can_produce_at(
                    world,
                    command.player_id,
                    command.target_x,
                    command.target_y,
                    command.unit_type,
                    &master,
                )
            })
            .map_err(IssueRejection::FacilityUnavailable)?;
    }
    Ok(())
}

/// 発行待ちの列から、検証を通る最初の購入案を取り出す。
///
/// 検証に失敗した案は発行せず `IssueAudit` へ記録して捨て、次の案を検証する。
/// 戻り値は発行する案と、取り消した案（呼び出し側が版別の予約を解く）の組。
pub fn next_issuable<T>(
    world: &mut World,
    turn: u32,
    queue: &mut std::collections::VecDeque<T>,
    command_of: impl Fn(&T) -> &ProduceUnitCommand,
) -> (Option<T>, Vec<T>) {
    let mut cancelled = Vec::new();
    while let Some(candidate) = queue.pop_front() {
        match validate_issue(world, command_of(&candidate)) {
            Ok(()) => return (Some(candidate), cancelled),
            Err(rejection) => {
                let command = command_of(&candidate).clone();
                world
                    .get_resource_or_insert_with(IssueAudit::default)
                    .record(turn, command, rejection);
                cancelled.push(candidate);
            }
        }
    }
    (None, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{GridPosition, PlayerId, Property};
    use crate::resources::{Player, Terrain, UnitType};
    use std::collections::VecDeque;
    /// プレイヤーIDから現在資金を読む。
    fn funds_of(world: &World, player_id: PlayerId) -> Option<u32> {
        world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)
    }

    fn world_with_funds(funds: u32) -> World {
        let mut world = World::new();
        world.insert_resource(Players(vec![Player {
            id: PlayerId(1),
            name: "P1".to_string(),
            funds,
        }]));
        world
    }

    /// 軽歩兵だけを登録した能力値表。
    fn infantry_registry() -> UnitRegistry {
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: crate::resources::MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 10,
            ammo1_cost: 0,
            max_ammo2: 0,
            ammo2_cost: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        };
        UnitRegistry(crate::collections::HashMap::from_iter([(
            UnitType::Infantry,
            stats,
        )]))
    }

    fn command(x: usize, unit_type: UnitType) -> ProduceUnitCommand {
        ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: x,
            target_y: 0,
            unit_type,
        }
    }

    #[test]
    fn rejects_unknown_player() {
        let mut world = world_with_funds(0);
        let mut cmd = command(0, UnitType::Infantry);
        cmd.player_id = PlayerId(9);
        assert_eq!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::UnknownPlayer)
        );
    }

    #[test]
    fn rejects_when_funds_dropped_after_planning() {
        let mut world = world_with_funds(500);
        world.insert_resource(infantry_registry());
        let rejection = validate_issue(&mut world, &command(0, UnitType::Infantry));
        assert!(matches!(
            rejection,
            Err(IssueRejection::InsufficientFunds {
                required: 1000,
                available: 500
            })
        ));
        assert_eq!(funds_of(&world, PlayerId(1)), Some(500));
    }

    #[test]
    fn rejects_lost_facility_and_issues_next_valid_order() {
        let master = MasterDataRegistry::load().expect("マスターデータ");
        let mut world = world_with_funds(10_000);
        world.insert_resource(infantry_registry());
        world.insert_resource(master);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(PlayerId(1)), 100),
        ));
        // x=1 の工場は敵に奪われた
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(2)), 100),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(1)), 100),
        ));
        let mut queue = VecDeque::from(vec![
            command(1, UnitType::Infantry),
            command(2, UnitType::Infantry),
        ]);
        let (issued, cancelled) = next_issuable(&mut world, 3, &mut queue, |cmd| cmd);
        assert_eq!(issued.map(|cmd| cmd.target_x), Some(2));
        assert_eq!(cancelled.len(), 1);
        assert!(queue.is_empty());
        let audit = world.resource::<IssueAudit>();
        assert_eq!(audit.rejected().len(), 1);
        assert_eq!(audit.rejected()[0].turn, 3);
        assert!(matches!(
            audit.rejected()[0].rejection,
            IssueRejection::FacilityUnavailable(_)
        ));
    }
}
