//! 遊兵（＝ミッションを持たない／ミッション通りに動けていないユニット）の計測。
//!
//! V4 生産AI 修正計画の一次指標「遊兵ゼロ」を実装する。
//! 勝敗だけで修正の成否を測ると原因の切り分けができないため、
//! 「そのターン、任務が無かった／任務はあるのに動かなかったユニット」を毎ターン数える。
//!
//! 計測は `execute_ai_turn_v2` がターン終了（`NextPhaseCommand`）を送る直前で行う。
//! V2 系AIは1呼び出し1行動のステップ実行で、行動済み Entity は `AiActionCooldown`
//! （ターン境界で破棄される per-turn リソース）に溜まるため、
//! 「そのターンに何が動かなかったか」が確定するのはこの一点だけである。
//!
//! ここでは新しい永続状態を持たない。分類 D（停滞 Squad：フェーズが N ターン進まない）は
//! Squad ダイジェストのターン間差分で求められるよう、判定材料だけを出力に載せる。

use crate::ai::islands::IslandId;
use crate::ai::squad::{MissionPhase, MissionType, SquadId, SquadManager};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, Transporting, UnitStats,
};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 遊兵1体分の記録。分類は排他ではなく、同時に複数立つ（C は A・B の上位集合）。
#[derive(Debug, Clone)]
pub struct IdleUnitRecord {
    pub entity: Entity,
    pub unit_type: UnitType,
    pub position: GridPosition,
    /// 所属している Squad（分類 A の場合は None）。
    pub squad_id: Option<SquadId>,
    /// 分類A: どの Squad の構成要素でもなく、単独行動の対象にも入っていない。
    pub no_mission: bool,
    /// 分類B: Squad には属しているが、そのターン一度も行動しなかった。
    pub mission_stalled: bool,
    /// 分類C: 行動可能（未移動かつ未行動完了）なままターンを終えた。
    pub actionable: bool,
}

/// Squad 単位のダイジェスト。分類 D はこれのターン間差分で判定する。
#[derive(Debug, Clone)]
pub struct IdleSquadDigest {
    pub squad_id: SquadId,
    pub mission_type: MissionType,
    pub phase: MissionPhase,
    pub target: Option<GridPosition>,
    pub target_island: Option<IslandId>,
    /// 対象プレイヤーの構成 Entity 数（輸送役・カーゴ・降車済みを含む）。
    pub member_count: usize,
    /// そのターンに1回でも行動した構成 Entity 数。0 が続けば停滞している。
    pub acted_count: usize,
}

/// 1プレイヤー・1ターン分の遊兵計測結果。
#[derive(Debug, Clone)]
pub struct IdleAudit {
    pub player_id: PlayerId,
    /// 母数（盤上に実体がある自軍ユニット数）。輸送中のユニットは含まない。
    pub total_units: usize,
    /// いずれかの分類に該当したユニットのみを保持する。
    pub records: Vec<IdleUnitRecord>,
    /// 自軍ユニットを1体以上含む Squad のダイジェスト。
    pub squads: Vec<IdleSquadDigest>,
}

impl IdleAudit {
    /// 分類A（任務なし）の数。
    pub fn no_mission_count(&self) -> usize {
        self.records.iter().filter(|r| r.no_mission).count()
    }

    /// 分類B（任務はあるが命令が出ない）の数。
    pub fn mission_stalled_count(&self) -> usize {
        self.records.iter().filter(|r| r.mission_stalled).count()
    }

    /// 分類C（行動可能なまま終了）の数。
    pub fn actionable_count(&self) -> usize {
        self.records.iter().filter(|r| r.actionable).count()
    }
}

/// 直近ターンの計測結果をプレイヤー別に保持する診断用リソース。
/// 各プレイヤーのターン終了時に自分の分だけを上書きするため、履歴は持たない。
#[derive(Resource, Debug, Clone, Default)]
pub struct IdleAuditDiagnostics {
    pub by_player: HashMap<PlayerId, IdleAudit>,
}

impl IdleAuditDiagnostics {
    pub fn record(&mut self, audit: IdleAudit) {
        self.by_player.insert(audit.player_id, audit);
    }
}

/// `player_id` の遊兵を分類 A/B/C に集計する。
///
/// `acted` はそのターンに行動した Entity の集合（`AiActionCooldown` の中身）。
/// 盤面の状態だけから導出する純粋な関数で、World を書き換えない。
pub fn audit_idle_units(
    world: &mut World,
    player_id: PlayerId,
    acted: &HashSet<Entity>,
) -> IdleAudit {
    // 1. Squad 側の情報を所有値へ写し取る。
    //    このあと World へクエリを掛けるためにリソースの借用をここで手放す必要がある。
    let mut assigned: HashMap<Entity, SquadId> = HashMap::new();
    let mut squad_members: HashMap<SquadId, Vec<Entity>> = HashMap::new();
    // 構成員の集計（member_count / acted_count）はクエリ後に埋めるため、ここでは 0 で作る。
    let mut squad_digests: Vec<IdleSquadDigest> = Vec::new();
    let mut solo_fallbacks: HashSet<Entity> = HashSet::new();

    if let Some(manager) = world.get_resource::<SquadManager>() {
        solo_fallbacks = manager.solo_fallbacks.clone();
        for squad in &manager.squads {
            // 構成要素は members だけではない。輸送役・カーゴ・降車待ちも「任務を持っている」。
            let mut entities: Vec<Entity> = squad.members.iter().copied().collect();
            if let Some(transport) = squad.transport_entity {
                entities.push(transport);
            }
            entities.extend(squad.cargo_entities.iter().copied());
            entities.extend(squad.delivered_cargo.iter().copied());
            entities.sort_by_key(|entity| entity.to_bits());
            entities.dedup();

            for entity in &entities {
                // ユニットは1勢力にしか属さないため、他プレイヤーの Squad が
                // 自軍ユニットを含むことはない。所有者での事前フィルタは不要。
                assigned.entry(*entity).or_insert(squad.id);
            }

            squad_digests.push(IdleSquadDigest {
                squad_id: squad.id,
                mission_type: squad.mission_type.clone(),
                phase: squad.phase.clone(),
                target: squad.target,
                target_island: squad.target_island,
                member_count: 0,
                acted_count: 0,
            });
            squad_members.insert(squad.id, entities);
        }
    }

    // 2. 盤上の自軍ユニットを走査して分類する。
    let mut records: Vec<IdleUnitRecord> = Vec::new();
    let mut own_units: HashSet<Entity> = HashSet::new();
    let mut total_units = 0usize;

    let mut query = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        &HasMoved,
        &ActionCompleted,
        Option<&Transporting>,
    )>();
    for (entity, faction, position, stats, has_moved, action_completed, transporting) in
        query.iter(world)
    {
        if faction.0 != player_id {
            continue;
        }
        // 輸送中のユニットは盤上に実体がなく自力で動けないため母数から除く。
        if transporting.is_some() {
            continue;
        }

        total_units += 1;
        own_units.insert(entity);

        let squad_id = assigned.get(&entity).copied();
        let has_acted = acted.contains(&entity);

        // A: どの Squad にも属さず、単独行動の対象にもなっていない。
        let no_mission = squad_id.is_none() && !solo_fallbacks.contains(&entity);
        // B: Squad には属するが、そのターン一度も命令が出なかった。
        let mission_stalled = squad_id.is_some() && !has_acted;
        // C: 行動可能なままターンが終わった（A・B の観測可能な上位集合）。
        let actionable = !has_moved.0 && !action_completed.0 && !has_acted;

        if no_mission || mission_stalled || actionable {
            records.push(IdleUnitRecord {
                entity,
                unit_type: stats.unit_type,
                position: *position,
                squad_id,
                no_mission,
                mission_stalled,
                actionable,
            });
        }
    }

    // 3. 自軍ユニットを含む Squad だけをダイジェスト化する（分類 D の判定材料）。
    let mut squads: Vec<IdleSquadDigest> = squad_digests
        .into_iter()
        .filter_map(|mut digest| {
            let entities = squad_members.get(&digest.squad_id)?;
            let owned: Vec<Entity> = entities
                .iter()
                .copied()
                .filter(|entity| own_units.contains(entity))
                .collect();
            if owned.is_empty() {
                return None;
            }
            digest.member_count = owned.len();
            digest.acted_count = owned.iter().filter(|entity| acted.contains(entity)).count();
            Some(digest)
        })
        .collect();

    // トレース出力を再現可能にするため決定的に並べる。
    records.sort_by_key(|record| record.entity.to_bits());
    squads.sort_by_key(|digest| digest.squad_id.0);

    IdleAudit {
        player_id,
        total_units,
        records,
        squads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Ammo, Fuel, Health};
    use crate::resources::{GridTopology, Map, Terrain};

    fn setup_world() -> World {
        let master_data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        // 既存ユニットを取り除き、テストで置いたユニットだけを母数にする。
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }
        let map = Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: GridTopology::Square,
        };
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(map);
        world
    }

    fn spawn_unit(world: &mut World, player: PlayerId, x: usize, y: usize) -> Entity {
        world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x, y },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..Default::default()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id()
    }

    /// Squad を1つも持たない自軍ユニットは分類A（任務なし）に計上される。
    #[test]
    fn unit_without_squad_is_counted_as_no_mission() {
        let mut world = setup_world();
        let player = PlayerId(1);
        let entity = spawn_unit(&mut world, player, 1, 1);

        let audit = audit_idle_units(&mut world, player, &HashSet::new());

        assert_eq!(audit.total_units, 1);
        assert_eq!(audit.no_mission_count(), 1);
        assert_eq!(audit.mission_stalled_count(), 0);
        // 未行動なので分類Cにも入る（A の上位集合であること）。
        assert_eq!(audit.actionable_count(), 1);
        assert_eq!(audit.records[0].entity, entity);
        assert_eq!(audit.records[0].unit_type, UnitType::Infantry);
        assert!(audit.squads.is_empty());
    }

    /// 輸送 Squad に属しながらそのターン行動しなかったユニットは分類B（命令が出ない）になる。
    #[test]
    fn squad_member_without_action_is_counted_as_mission_stalled() {
        let mut world = setup_world();
        let player = PlayerId(1);
        let transport = spawn_unit(&mut world, player, 1, 1);
        let cargo = spawn_unit(&mut world, player, 1, 2);

        let mut manager = SquadManager::new();
        {
            let squad = manager.create_owned_squad(MissionType::Transport, player);
            squad.transport_entity = Some(transport);
            squad.cargo_entities = vec![cargo];
            squad.phase = MissionPhase::Transport(crate::ai::squad::TransportPhase::Pickup);
        }
        world.insert_resource(manager);

        // 輸送役だけが行動したターンを想定する。
        let acted: HashSet<Entity> = [transport].into_iter().collect();
        let audit = audit_idle_units(&mut world, player, &acted);

        assert_eq!(audit.total_units, 2);
        // 両方とも Squad に属するので分類Aは0。
        assert_eq!(audit.no_mission_count(), 0);
        // 動かなかったカーゴだけが分類B。
        assert_eq!(audit.mission_stalled_count(), 1);
        let stalled = audit
            .records
            .iter()
            .find(|record| record.mission_stalled)
            .expect("停滞ユニットが計上されること");
        assert_eq!(stalled.entity, cargo);
        assert_eq!(stalled.squad_id, Some(SquadId(0)));

        // 分類D判定用のダイジェストが1件出ること。
        assert_eq!(audit.squads.len(), 1);
        assert_eq!(audit.squads[0].member_count, 2);
        assert_eq!(audit.squads[0].acted_count, 1);
    }

    /// 輸送中（Transporting）のユニットは自力で動けないため母数から除外する。
    #[test]
    fn transported_unit_is_excluded_from_audit() {
        let mut world = setup_world();
        let player = PlayerId(1);
        let transport = spawn_unit(&mut world, player, 1, 1);
        let cargo = spawn_unit(&mut world, player, 1, 2);
        world.entity_mut(cargo).insert(Transporting(transport));

        let audit = audit_idle_units(&mut world, player, &HashSet::new());

        assert_eq!(audit.total_units, 1);
        assert_eq!(audit.no_mission_count(), 1);
        assert!(audit.records.iter().all(|record| record.entity != cargo));
    }
}
