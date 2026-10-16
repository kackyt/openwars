//! Squadのデータ型と限定編集API。
//!
//! rosterは所属提案の唯一の保存先。managerは一覧構造、SquadEditは実行計画、
//! ledgerの各commitは所属集合を更新する。一覧から退避したdraftも自身のrosterを保持する。

use crate::ai::islands::IslandId;
use crate::ai::operation_assignment::OperationUnitRole;
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;
use std::ops::Deref;

/// ミッションの種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionType {
    Attack,
    Capture,
    Defense,
    /// 直ちに投入できる作戦が無い間も、再使用可能な位置へ明示配置する予備任務。
    /// `solo_fallback`と異なり目標座標を固定し、次のcampaign再分析で再徴用できる。
    Reserve,
    Transport,
}

/// 輸送ミッションの各フェーズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    Pickup,
    Transit,
    Drop,
    Return,
}

/// 部隊が実行中のミッションの状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionPhase {
    Forming,
    MovingToTarget,
    Executing,
    Completed,
    Transport(TransportPhase),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquadId(pub u32);

/// Squadの再配置・複製でも維持する実行作戦identity。DAG指令はmember集合ではなくこれを参照する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SquadOperationId(pub u64);

/// 部隊（Squad）の定義
///
/// 所属の実行binding（通常メンバー・輸送役・積荷・降車済み積荷）は、このmoduleだけが
/// 名前を付けられる非公開field `roster` に保持する。外部からは [`Deref`] で
/// `squad.members` のように読めるが、`DerefMut` が無いためfield代入・swap・moveはできず、
/// 本番の更新はmanager単位の [`ProposalCommit`] と、明示的なdraftの確定で行う。
/// `Clone` は手実装する（所属投影型は `.clone()` が中身の写しを返すよう `Clone` を持たない）。
///
/// 所属は読める:
///
/// ```
/// fn size(squad: &engine::ai::squad::Squad) -> usize {
///     squad.members.len() + squad.cargo_entities.len() + usize::from(squad.transport_entity.is_some())
/// }
/// ```
///
/// 所属fieldへの代入（既定値での上書きを含む）はできない:
///
/// ```compile_fail,E0594
/// fn clear(squad: &mut engine::ai::squad::Squad) {
///     squad.members = Default::default();
/// }
/// ```
///
/// ```compile_fail,E0594
/// fn unbind(squad: &mut engine::ai::squad::Squad) {
///     squad.transport_entity = Default::default();
/// }
/// ```
///
/// 2つのSquad間の所属のswap・取り出しもできない:
///
/// ```compile_fail,E0596
/// fn swap(a: &mut engine::ai::squad::Squad, b: &mut engine::ai::squad::Squad) {
///     std::mem::swap(&mut a.cargo_entities, &mut b.cargo_entities);
/// }
/// ```
///
/// ```compile_fail,E0596
/// fn take(squad: &mut engine::ai::squad::Squad) {
///     let _ = std::mem::take(&mut squad.delivered_cargo);
/// }
/// ```
///
/// 中身の集合も変更できない:
///
/// ```compile_fail,E0596
/// fn join(squad: &mut engine::ai::squad::Squad, entity: bevy_ecs::entity::Entity) {
///     squad.members.insert(entity);
/// }
/// ```
#[derive(Debug)]
pub struct Squad {
    pub id: SquadId,
    /// 実行形の再配置でも変えない論理作戦参照。新設時だけ発番する。
    pub(crate) operation_id: SquadOperationId,
    pub plan: SquadPlan,
    /// 再計画中に一時解放したReserveの受入identity。所属の正本ではない。
    pub(crate) reserve_entity: Option<Entity>,
    pub(super) roster: SquadRoster,
}

/// 所属とは別の実行計画。変更しても一覧や所属集合を置換しない。
#[derive(Debug, Clone)]
pub struct SquadPlan {
    /// Entityがまだ無いFormingもplayer間で共有しない、Squad自身の明示所有者。
    pub owner_id: Option<PlayerId>,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>, // 攻撃・防衛・占領の目標座標
    pub target_island: Option<IslandId>, // 輸送ターゲットの島
    pub phase: MissionPhase,
    /// Pickup 時の合流位置。
    pub pickup_position: Option<GridPosition>,
    /// Transit/Drop で最後に選択した降車位置。
    pub drop_position: Option<GridPosition>,
    /// 中立島・兵站島への便は、搭載済みcargoを遠い後続cargo待ちで止めず逐次発進できる。
    /// 敵領Assaultは侵攻波の分断を避けるためfalseを維持する。
    pub allow_partial_departure: bool,
    /// 敵領Assaultの形成中もLoadまでは進めるが、上位の兵站gateが開くまで
    /// Transitへ移らない。単なるForming待機と搭載済み発進待ちを区別する。
    pub departure_authorized: bool,
    /// 着陸地点の軽歩兵だけを排除した後、輸送任務のReturnへ復帰する一時護衛状態。
    pub return_after_combat: bool,
}

/// Squad本体を置換できない編集ハンドル。
///
/// 計画は更新できる:
/// ```
/// use engine::ai::squad::{SquadManager, MissionType, MissionPhase};
/// let mut manager = SquadManager::new();
/// let mut edit = manager.create_squad(MissionType::Attack);
/// edit.plan_mut().phase = MissionPhase::Completed;
/// ```
///
/// 参照先のSquad自体は置換できない:
/// ```compile_fail,E0594
/// use engine::ai::squad::{Squad, SquadEdit};
/// fn replace(edit: &mut SquadEdit<'_>, other: Squad) { **edit = other; }
/// ```
/// ```compile_fail,E0596
/// use engine::ai::squad::{Squad, SquadEdit};
/// fn take(edit: &mut SquadEdit<'_>, other: Squad) {
///     std::mem::replace(&mut **edit, other);
/// }
/// ```
/// managerから生の可変参照は取り出せない:
/// ```compile_fail,E0308
/// use engine::ai::squad::{Squad, SquadManager, MissionType};
/// let mut manager = SquadManager::new();
/// let raw: &mut Squad = manager.create_squad(MissionType::Attack);
/// ```
/// ```compile_fail,E0599
/// use engine::ai::squad::SquadManager;
/// let mut manager = SquadManager::new();
/// manager.squads_mut().reverse();
/// ```
pub struct SquadEdit<'a>(&'a mut Squad);

impl std::fmt::Debug for SquadEdit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0, f)
    }
}

impl Deref for SquadEdit<'_> {
    type Target = Squad;
    fn deref(&self) -> &Squad {
        self.0
    }
}

impl SquadEdit<'_> {
    pub fn plan_mut(&mut self) -> &mut SquadPlan {
        &mut self.0.plan
    }

    /// Reserve再利用用の履歴だけを更新する。現在所属の変更ではない。
    pub(crate) fn remember_reserve(&mut self, entity: Option<Entity>) {
        self.0.reserve_entity = entity;
    }
}

/// Squadの所属実行binding。読取りは `Squad` のDeref経由で行う。
///
/// `Debug` は所属の4項目だけを出力する。
#[derive(Default)]
pub struct SquadRoster {
    /// 部隊メンバー。HashSet はプロセス・スレッドごとに反復順が変わり、
    /// 「先頭メンバーの位置」を基準にする探索が同一seedでも再現しなくなるため、
    /// Entity 順で安定する BTreeSet を用いる。
    pub members: MemberRoster,
    /// 輸送部隊の輸送役。HashSet の列挙順ではなく明示的に保持する。
    pub transport_entity: TransportBinding,
    /// この輸送部隊へ割り当てたカーゴ。占領要員を先頭にした決定的な順序で保持する。
    pub cargo_entities: CargoRoster,
    /// 降車済みで通常部隊への引き継ぎを待つカーゴ。
    pub delivered_cargo: CargoRoster,
}

impl std::fmt::Debug for SquadRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SquadRoster")
            .field("members", &self.members)
            .field("transport_entity", &self.transport_entity)
            .field("cargo_entities", &self.cargo_entities)
            .field("delivered_cargo", &self.delivered_cargo)
            .finish()
    }
}

impl Squad {
    /// 一覧から退避した所有draftを編集する。manager内への可変参照は公開しない。
    pub(crate) fn edit(&mut self) -> SquadEdit<'_> {
        SquadEdit(self)
    }

    /// 所属が空のSquadを作る（`SquadManager::create_squad` と同じ既定値）。
    /// 所属は作成後に [`RosterCommit`] から登録する。
    pub(crate) fn new(id: SquadId, owner_id: Option<PlayerId>, mission_type: MissionType) -> Self {
        Self {
            id,
            operation_id: SquadOperationId(u64::from(id.0)),
            plan: SquadPlan {
                owner_id,
                mission_type,
                target: None,
                target_island: None,
                phase: MissionPhase::Forming,
                pickup_position: None,
                drop_position: None,
                allow_partial_departure: false,
                departure_authorized: true,
                return_after_combat: false,
            },
            reserve_entity: None,
            roster: SquadRoster {
                ..SquadRoster::default()
            },
        }
    }
}

impl Deref for Squad {
    type Target = SquadRoster;
    fn deref(&self) -> &Self::Target {
        &self.roster
    }
}

impl Clone for Squad {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            operation_id: self.operation_id,
            plan: self.plan.clone(),
            reserve_entity: self.reserve_entity,
            roster: SquadRoster {
                members: self.roster.members.duplicate(),
                transport_entity: self.roster.transport_entity,
                cargo_entities: self.roster.cargo_entities.duplicate(),
                delivered_cargo: self.roster.delivered_cargo.duplicate(),
            },
        }
    }
}

/// Squadの通常メンバー（実行binding）。Entity順で安定するBTreeSetを読取り専用で公開する。
///
/// 書込みは [`RosterCommit`] だけが行う。テストの盤面組立てだけは可変参照を許す。
///
/// `Clone` を実装しないのは意図的である。`squad.members.clone()` は従来どおり
/// 中身の `BTreeSet` 複製（読取りの写し）になり、Squad全体の複製は [`Self::duplicate`] で行う。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MemberRoster(pub(super) BTreeSet<Entity>);

/// 積荷・降車済み積荷の実行binding。占領要員を先頭にした明示順を保持する。
///
/// `MemberRoster` と同じ理由で `Clone` を実装しない（`.clone()` は `Vec` の写し）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CargoRoster(pub(super) Vec<Entity>);

impl MemberRoster {
    /// Squad全体の複製（simulation・候補Squadの退避）でだけ使う。
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

impl CargoRoster {
    /// Squad全体の複製でだけ使う。
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

/// 輸送Squadの輸送役binding。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportBinding(pub(super) Option<Entity>);

impl Deref for MemberRoster {
    type Target = BTreeSet<Entity>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for CargoRoster {
    type Target = Vec<Entity>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for TransportBinding {
    type Target = Option<Entity>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a MemberRoster {
    type Item = &'a Entity;
    type IntoIter = std::collections::btree_set::Iter<'a, Entity>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a CargoRoster {
    type Item = &'a Entity;
    type IntoIter = std::slice::Iter<'a, Entity>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl PartialEq<Option<Entity>> for TransportBinding {
    fn eq(&self, other: &Option<Entity>) -> bool {
        self.0 == *other
    }
}

impl PartialEq<BTreeSet<Entity>> for MemberRoster {
    fn eq(&self, other: &BTreeSet<Entity>) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Vec<Entity>> for CargoRoster {
    fn eq(&self, other: &Vec<Entity>) -> bool {
        self.0 == *other
    }
}

/// 1つのSquadの実行bindingへの差分確定。所属を変える書込みはすべてここを通す。
///
/// 正規化前の候補登録であり、一意所属の解決は [`AssignmentProposal::resolve`] が行う。
/// 各操作は従来の直接書込みと同じ順序・同じ集合演算で適用する（R）。
pub(crate) struct RosterCommit<'a> {
    squad: &'a mut SquadRoster,
}

impl<'a> RosterCommit<'a> {
    pub(super) fn open(squad: &'a mut SquadEdit<'_>) -> Self {
        Self {
            squad: &mut squad.0.roster,
        }
    }
}

impl RosterCommit<'_> {
    /// 通常メンバーへ追加する。追加された場合はtrue。
    #[cfg(test)]
    pub(crate) fn join(&mut self, entity: Entity) -> bool {
        self.squad.members.0.insert(entity)
    }

    pub(crate) fn join_all(&mut self, entities: impl IntoIterator<Item = Entity>) {
        self.squad.members.0.extend(entities);
    }

    /// 輸送役bindingを設定する。
    pub(crate) fn bind_transport(&mut self, transport: Option<Entity>) {
        self.squad.transport_entity.0 = transport;
    }

    /// 条件に合うEntityを通常メンバー・積荷・降車済み積荷から外し、
    /// 輸送役bindingも該当すれば外す（従来の3つのretain＋輸送役解除と同じ順序）。
    ///
    /// 述語は同じEntityに対して最大4回（members・cargo・delivered・transport）呼ばれる。
    /// 呼出し間で結果が変わる述語（状態を持つclosure等）を渡してはならない。
    #[cfg(test)]
    pub(crate) fn release_where(&mut self, mut release: impl FnMut(&Entity) -> bool) {
        self.squad.members.0.retain(|entity| !release(entity));
        self.squad
            .cargo_entities
            .0
            .retain(|entity| !release(entity));
        self.squad
            .delivered_cargo
            .0
            .retain(|entity| !release(entity));
        if self.squad.transport_entity.0.is_some_and(|t| release(&t)) {
            self.squad.transport_entity.0 = None;
        }
    }

    /// 輸送役1体と積荷列だけを持つ輸送波へbindingを置き換える。
    /// 降車済み積荷は変更しない（呼出し側が従来どおり個別に扱う）。
    pub(crate) fn rebind_transport_wave(&mut self, transport: Entity, cargo: Vec<Entity>) {
        self.squad.members.0.clear();
        self.squad.members.0.insert(transport);
        self.squad.transport_entity.0 = Some(transport);
        self.squad.cargo_entities.0 = cargo;
    }

    /// 確定中の積荷bindingに含まれるか（確定口を開いたまま読むための参照）。
    pub(crate) fn has_cargo(&self, entity: Entity) -> bool {
        self.squad.cargo_entities.0.contains(&entity)
    }

    pub(crate) fn push_cargo(&mut self, entity: Entity) {
        self.squad.cargo_entities.0.push(entity);
    }

    pub(crate) fn extend_cargo(&mut self, entities: impl IntoIterator<Item = Entity>) {
        self.squad.cargo_entities.0.extend(entities);
    }

    pub(crate) fn retain_cargo(&mut self, keep: impl FnMut(&Entity) -> bool) {
        self.squad.cargo_entities.0.retain(keep);
    }

    pub(crate) fn replace_cargo(&mut self, cargo: Vec<Entity>) {
        self.squad.cargo_entities.0 = cargo;
    }

    /// 積荷bindingを取り出して空にする。呼出し側は残す積荷を [`Self::replace_cargo`] で戻す。
    pub(crate) fn take_cargo(&mut self) -> Vec<Entity> {
        std::mem::take(&mut self.squad.cargo_entities.0)
    }

    /// 積荷をEntity bit順へ揃えて重複を除く（従来の `sort_by_key(to_bits)` + `dedup`）。
    pub(crate) fn sort_dedup_cargo(&mut self) {
        self.squad
            .cargo_entities
            .0
            .sort_by_key(|entity| entity.to_bits());
        self.squad.cargo_entities.0.dedup();
    }

    pub(crate) fn push_delivered(&mut self, entity: Entity) {
        self.squad.delivered_cargo.0.push(entity);
    }

    pub(crate) fn clear_delivered(&mut self) {
        self.squad.delivered_cargo.0.clear();
    }
}

/// テストの盤面組立てでは従来どおり直接書けるようにする。
/// 本番buildにはDerefMutが無く、所属の書込みはRosterCommitへ限定される。
#[cfg(test)]
mod fixture_access {
    use super::*;
    use std::ops::DerefMut;

    /// テストの盤面組立てだけは `squad.members.insert(..)` のように直接書ける。
    impl DerefMut for SquadEdit<'_> {
        fn deref_mut(&mut self) -> &mut Squad {
            self.0
        }
    }

    impl DerefMut for Squad {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.roster
        }
    }

    impl DerefMut for MemberRoster {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl DerefMut for CargoRoster {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl DerefMut for TransportBinding {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl From<BTreeSet<Entity>> for MemberRoster {
        fn from(value: BTreeSet<Entity>) -> Self {
            Self(value)
        }
    }

    impl From<Vec<Entity>> for CargoRoster {
        fn from(value: Vec<Entity>) -> Self {
            Self(value)
        }
    }

    impl From<Option<Entity>> for TransportBinding {
        fn from(value: Option<Entity>) -> Self {
            Self(value)
        }
    }
}

/// 全ての部隊を管理するリソース
///
/// Squad一覧はこのmoduleの非公開fieldにあり、`manager.squads` はDeref経由の読取りだけ。
/// 一覧の各Squadの非所属field（任務・目標・phase等）の更新は [`Self::edit_each`] で行う。
/// 一覧の構造変更（解散・退避・復元・発番済み輸送波の追加）は
/// `OperationLedger::squad_list` の [`SquadListCommit`] だけが行う。
///
/// 一覧は読める:
///
/// ```
/// fn count(manager: &engine::ai::squad::SquadManager) -> usize {
///     manager.squads.iter().filter(|squad| !squad.members.is_empty()).count()
/// }
/// ```
///
/// 一覧の置換・解散・追加はできない:
///
/// ```compile_fail,E0594
/// fn reset(manager: &mut engine::ai::squad::SquadManager) {
///     manager.squads = Vec::new();
/// }
/// ```
///
/// ```compile_fail,E0596
/// fn disband(manager: &mut engine::ai::squad::SquadManager) {
///     manager.squads.retain(|squad| squad.members.is_empty());
/// }
/// ```
///
/// ```compile_fail,E0596
/// fn take(manager: &mut engine::ai::squad::SquadManager) {
///     let _ = std::mem::take(&mut manager.squads);
/// }
/// ```
#[derive(Resource, Default, Debug, Clone)]
pub struct SquadManager {
    list: SquadList,
    pub solo_fallbacks: HashSet<Entity>,
    next_id: u32,
}

/// `SquadManager` の読取りview。
#[derive(Default, Debug, Clone)]
pub struct SquadList {
    pub squads: Vec<Squad>,
}

impl Deref for SquadManager {
    type Target = SquadList;
    fn deref(&self) -> &Self::Target {
        &self.list
    }
}

impl SquadManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 所属が空のSquadを発番して追加する。
    pub fn create_squad(&mut self, mission_type: MissionType) -> SquadEdit<'_> {
        let squad = Squad::new(SquadId(self.next_id), None, mission_type);
        self.next_id += 1;
        self.list.squads.push(squad);
        SquadEdit(self.list.squads.last_mut().unwrap())
    }

    pub fn create_owned_squad(
        &mut self,
        mission_type: MissionType,
        owner_id: PlayerId,
    ) -> SquadEdit<'_> {
        let mut squad = self.create_squad(mission_type);
        squad.plan_mut().owner_id = Some(owner_id);
        squad
    }

    /// 正規化前の複数候補を含む編成用view。現在所属の照会にはregistryを使う。
    pub(crate) fn proposal_view(&self) -> &SquadList {
        &self.list
    }

    /// 一覧順に限定ハンドルを返す。ハンドルを並べ替えても一覧本体は変わらない。
    pub(crate) fn edit_each(&mut self) -> impl Iterator<Item = SquadEdit<'_>> {
        self.list.squads.iter_mut().map(SquadEdit)
    }

    pub(super) fn list_commit(&mut self) -> SquadListCommit<'_> {
        SquadListCommit { manager: self }
    }

    /// 編成途中に参照されるEntity。現在の一意所属の照会には使わない。
    pub(crate) fn mentioned_entities(&self) -> HashSet<Entity> {
        self.list
            .squads
            .iter()
            .flat_map(|s| {
                s.members
                    .iter()
                    .copied()
                    .chain(s.transport_entity.iter().copied())
                    .chain(s.cargo_entities.iter().copied())
                    .chain(s.delivered_cargo.iter().copied())
            })
            .collect()
    }

    pub(super) fn proposal_commit(&mut self) -> ProposalCommit<'_> {
        ProposalCommit { manager: self }
    }
}

/// SquadIdで指す所属提案の更新口。rosterを直接更新する。
/// 書込みのたびに一意化はしない（一意化は正規化resolveの責務）。
pub(crate) struct ProposalCommit<'a> {
    manager: &'a mut SquadManager,
}

impl ProposalCommit<'_> {
    fn index_of(&self, squad_id: SquadId) -> Option<usize> {
        self.manager
            .list
            .squads
            .iter()
            .position(|squad| squad.id == squad_id)
    }

    /// 所属提案を持つSquadを新設する。非所属fieldは返したSquadへ設定する。
    pub(crate) fn create_owned(
        &mut self,
        mission_type: MissionType,
        owner_id: PlayerId,
        members: BTreeSet<Entity>,
        transport: Option<Entity>,
        cargo: Vec<Entity>,
    ) -> SquadEdit<'_> {
        let squad = self.manager.create_owned_squad(mission_type, owner_id);
        squad.0.roster.members.0 = members;
        squad.0.roster.transport_entity.0 = transport;
        squad.0.roster.cargo_entities.0 = cargo;
        squad
    }

    /// 単一rosterへ差分を適用する。一意化はresolve時に行う。
    fn update<R>(&mut self, id: SquadId, apply: impl FnOnce(&mut SquadRoster) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        Some(apply(&mut self.manager.list.squads[index].roster))
    }

    /// 既存Squadへ通常メンバーとして提案する。追加された場合はtrue。
    pub(crate) fn join(&mut self, squad_id: SquadId, entity: Entity) -> bool {
        let Some(index) = self.index_of(squad_id) else {
            return false;
        };
        let squad = &mut self.manager.list.squads[index];
        squad.roster.members.0.insert(entity)
    }

    /// 配備接続を別Squadへ移すため、通常メンバーの提案を外す。
    pub(crate) fn leave(&mut self, id: SquadId, entity: Entity) -> bool {
        self.update(id, |r| r.members.0.remove(&entity))
            .unwrap_or(false)
    }

    /// 配備の受入集合を空にする。積荷・輸送役の提案は変更しない。
    pub(crate) fn clear_members(&mut self, id: SquadId) {
        self.update(id, |r| r.members.0.clear());
    }

    /// 責務の通常メンバーを置き換える。
    pub(crate) fn replace_members(&mut self, id: SquadId, members: BTreeSet<Entity>) {
        self.update(id, |r| r.members.0 = members);
    }

    /// 責務移管の前に旧参照を外す。旧規則どおり輸送役binding自体は変更しない。
    pub(crate) fn remove_mentions(&mut self, id: SquadId, entities: &HashSet<Entity>) {
        self.update(id, |r| {
            r.members.0.retain(|e| !entities.contains(e));
            r.cargo_entities.0.retain(|e| !entities.contains(e));
            r.delivered_cargo.0.retain(|e| !entities.contains(e));
        });
    }

    /// 消滅・移管対象を全役割から解放する。述語は各参照につき一度だけ評価する。
    pub(crate) fn release_where(&mut self, id: SquadId, mut release: impl FnMut(&Entity) -> bool) {
        self.update(id, |r| {
            r.members.0.retain(|e| !release(e));
            r.cargo_entities.0.retain(|e| !release(e));
            r.delivered_cargo.0.retain(|e| !release(e));
            if r.transport_entity.0.is_some_and(|e| release(&e)) {
                r.transport_entity.0 = None;
            }
        });
    }
    /// fallbackへの離脱は通常メンバーだけ。積荷と輸送役bindingは旧規則を保つ。
    pub(crate) fn retain_live(&mut self, id: SquadId, live: &HashSet<Entity>) {
        self.release_where(id, |e| !live.contains(e));
    }

    pub(crate) fn clear_cargo(&mut self, id: SquadId) {
        self.update(id, |r| r.cargo_entities.0.clear());
    }

    pub(crate) fn deliver(&mut self, id: SquadId, entity: Entity) {
        self.update(id, |r| {
            if !r.delivered_cargo.0.contains(&entity) {
                r.delivered_cargo.0.push(entity);
                r.delivered_cargo.0.sort_by_key(|e| e.to_bits());
            }
        });
    }

    /// 輸送phaseなど複数fieldを扱う提案トランザクション。
    /// 対象Squadの限定編集ハンドルを渡し、作業コピーと書戻しを作らない。
    pub(crate) fn edit<R>(
        &mut self,
        id: SquadId,
        apply: impl FnOnce(&mut SquadEdit<'_>) -> R,
    ) -> Option<R> {
        let index = self.index_of(id)?;
        Some(apply(&mut SquadEdit(&mut self.manager.list.squads[index])))
    }

    /// resolveで選んだ現在bindingを提案正本へ反映する。foreign参照の保護はcallerが決める。
    pub(super) fn project_current(
        &mut self,
        id: SquadId,
        bound: impl Fn(&Entity, Option<OperationUnitRole>) -> bool,
    ) {
        self.update(id, |r| {
            r.members.0.retain(|e| bound(e, None));
            r.cargo_entities
                .0
                .retain(|e| bound(e, Some(OperationUnitRole::Cargo)));
            r.delivered_cargo
                .0
                .retain(|e| bound(e, Some(OperationUnitRole::DeliveredCargo)));
            if r.transport_entity
                .0
                .is_some_and(|e| !r.members.0.contains(&e))
            {
                r.transport_entity.0 = None;
            }
        });
    }

    /// 一覧順のSquadId（提案の添字をidへ写すため）。
    pub(crate) fn squad_ids(&self) -> Vec<SquadId> {
        self.manager
            .list
            .squads
            .iter()
            .map(|squad| squad.id)
            .collect()
    }

    /// 非所属fieldを更新するSquad（所属fieldは読取り専用のまま）。
    pub(crate) fn squad_mut(&mut self, squad_id: SquadId) -> Option<SquadEdit<'_>> {
        let index = self.index_of(squad_id)?;
        Some(SquadEdit(&mut self.manager.list.squads[index]))
    }
}

/// Squad一覧の構造変更の確定口。解散したSquadの所属bindingも同時に解放される。
pub(crate) struct SquadListCommit<'a> {
    manager: &'a mut SquadManager,
}

impl SquadListCommit<'_> {
    /// 条件を満たさないSquadを解散する（従来の `squads.retain`）。
    pub(crate) fn retain(&mut self, keep: impl FnMut(&Squad) -> bool) {
        self.manager.list.squads.retain(keep);
    }

    /// 一覧から取り出す（再partition中の退避）。
    pub(crate) fn remove(&mut self, index: usize) -> Squad {
        self.manager.list.squads.remove(index)
    }

    pub(crate) fn insert(&mut self, index: usize, squad: Squad) {
        self.manager.list.squads.insert(index, squad);
    }

    /// 退避・再構成したSquadを戻す。
    pub(crate) fn extend(&mut self, squads: impl IntoIterator<Item = Squad>) {
        self.manager.list.squads.extend(squads);
    }

    /// 発番済みSquadの末尾を一覧外へ取り出す（新規idだけを使う再構成用）。
    pub(crate) fn pop(&mut self) -> Option<Squad> {
        self.manager.list.squads.pop()
    }

    /// SquadId順へ並べ直す。
    pub(crate) fn sort_by_id(&mut self) {
        self.manager.list.squads.sort_by_key(|squad| squad.id.0);
    }
}

/// テストの盤面組立てだけは一覧を直接書ける。
#[cfg(test)]
impl std::ops::DerefMut for SquadManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.list
    }
}

#[cfg(test)]
mod proposal_contracts {
    use super::*;
    use crate::ai::planning::assignment::OperationLedger;

    /// transportがmembers未登録でも参照済みとし、重複は集合で集約する。
    #[test]
    fn mentioned_entities_includes_standalone_transport_and_all_roles() {
        let [transport, cargo, delivered] = [50, 51, 52].map(Entity::from_raw);
        let mut manager = SquadManager::new();
        let id = OperationLedger::proposals(&mut manager)
            .create_owned(
                MissionType::Transport,
                PlayerId(1),
                BTreeSet::from([cargo]),
                Some(transport),
                vec![cargo],
            )
            .id;
        OperationLedger::proposals(&mut manager).deliver(id, delivered);
        assert_eq!(
            manager.mentioned_entities(),
            HashSet::from_iter([transport, cargo, delivered])
        );
        let draft = OperationLedger::squad_list(&mut manager).remove(0);
        assert!(manager.mentioned_entities().is_empty());
        assert_eq!(draft.transport_entity, Some(transport));
        OperationLedger::squad_list(&mut manager).insert(0, draft);
        assert_eq!(
            manager.mentioned_entities(),
            HashSet::from_iter([transport, cargo, delivered])
        );
    }

    /// 述語を各参照につき一度だけ評価し、通常・積荷・降車済み・輸送役の順に解放する。
    #[test]
    fn keyed_release_evaluates_each_reference_once() {
        let [transport, cargo, delivered] = [1, 2, 3].map(Entity::from_raw);
        let mut manager = SquadManager::new();
        let id = {
            let mut proposals = OperationLedger::proposals(&mut manager);
            let id = proposals
                .create_owned(
                    MissionType::Transport,
                    PlayerId(1),
                    BTreeSet::from([transport]),
                    Some(transport),
                    vec![cargo],
                )
                .id;
            proposals.deliver(id, delivered);
            id
        };
        let mut visits = 0;
        OperationLedger::proposals(&mut manager).release_where(id, |_| {
            visits += 1;
            true
        });
        assert_eq!(visits, 4); // members・cargo・delivered・transport。
        let squad = &manager.squads[0];
        assert!(squad.members.is_empty());
        assert!(squad.cargo_entities.is_empty());
        assert!(squad.delivered_cargo.is_empty());
        assert!(squad.transport_entity.is_none());
        assert!(manager.mentioned_entities().is_empty());
    }

    /// 退避したdraftは所属を保持するが、一覧へ戻すまで未所属判定の集計には含めない。
    #[test]
    fn structural_commit_removes_and_restores_only_explicit_draft_proposals() {
        let entity = Entity::from_raw(9);
        let mut manager = SquadManager::new();
        let id = OperationLedger::proposals(&mut manager)
            .create_owned(
                MissionType::Attack,
                PlayerId(1),
                BTreeSet::from([entity]),
                None,
                vec![],
            )
            .id;
        let draft = OperationLedger::squad_list(&mut manager).remove(0);
        assert!(!manager.mentioned_entities().contains(&entity));
        OperationLedger::squad_list(&mut manager).insert(0, draft);
        assert!(manager.mentioned_entities().contains(&entity));
        assert_eq!(manager.squads[0].id, id);
        OperationLedger::squad_list(&mut manager).retain(|_| false);
        assert!(!manager.mentioned_entities().contains(&entity));
        assert!(manager.squads.is_empty());
    }

    /// 輸送phaseとcargoの役割変更を、同じrosterに反映する。
    #[test]
    fn keyed_phase_edit_updates_delivered_roster() {
        let [transport, cargo] = [20, 21].map(Entity::from_raw);
        let mut manager = SquadManager::new();
        let id = OperationLedger::proposals(&mut manager)
            .create_owned(
                MissionType::Transport,
                PlayerId(1),
                BTreeSet::from([transport]),
                Some(transport),
                vec![cargo],
            )
            .id;
        OperationLedger::proposals(&mut manager).edit(id, |draft| {
            let mut roster = OperationLedger::roster(draft);
            roster.replace_cargo(Vec::new());
            roster.push_delivered(cargo);
            draft.plan.phase = MissionPhase::Transport(
                crate::ai::planning::assignment::squad_model::TransportPhase::Return,
            );
        });
        assert!(manager.mentioned_entities().contains(&cargo));
        assert!(manager.squads[0].cargo_entities.is_empty());
        assert_eq!(manager.squads[0].delivered_cargo, vec![cargo]);
    }
}
