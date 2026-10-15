//! 所属台帳の実行binding層（Squadごとのroster）。
//!
//! Squadの所属fieldはこのmoduleの非公開fieldにあり、外部はDerefで読むだけである。
//! 書込みは [`RosterCommit`]（`OperationLedger::roster` が開く）と、親moduleの正規化commitだけ。

use crate::ai::islands::IslandId;
use crate::ai::operation_assignment::OperationUnitRole;
use crate::ai::squad::{MissionPhase, MissionType, SquadId};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;
use std::ops::Deref;

/// 部隊（Squad）の定義
///
/// 所属の実行binding（通常メンバー・輸送役・積荷・降車済み積荷）は、このmoduleだけが
/// 名前を付けられる非公開field `roster` に保持する。外部からは [`Deref`] で
/// `squad.members` のように読めるが、`DerefMut` が無いためfield代入・swap・moveはできず、
/// 書込みは `OperationLedger::roster` が返す [`RosterCommit`] だけが行う。
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
    pub(super) roster: SquadRoster,
}

/// Squadの所属実行binding。読取りは `Squad` のDeref経由で行う。
///
/// `Debug` は所属だけを出力し、提案storeとの同期管理（`unsynced`）は含めない。
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
    /// 提案storeへ未反映の書込み（未移行writerのRosterCommit・テストfixture・複製）があるか。
    /// keyed確定口（[`ProposalCommit`]）の書込みはstoreと同時に適用するので立てない。
    pub(super) unsynced: bool,
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
    /// 所属が空のSquadを作る（`SquadManager::create_squad` と同じ既定値）。
    /// 所属は作成後に [`RosterCommit`] から登録する。
    pub(crate) fn new(id: SquadId, owner_id: Option<PlayerId>, mission_type: MissionType) -> Self {
        Self {
            id,
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
            roster: SquadRoster {
                // 一覧外で作られたSquadは、一覧へ入った時点で提案storeへ登録する。
                unsynced: true,
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
            owner_id: self.owner_id,
            mission_type: self.mission_type.clone(),
            target: self.target,
            target_island: self.target_island,
            phase: self.phase.clone(),
            pickup_position: self.pickup_position,
            drop_position: self.drop_position,
            allow_partial_departure: self.allow_partial_departure,
            departure_authorized: self.departure_authorized,
            return_after_combat: self.return_after_combat,
            roster: SquadRoster {
                members: self.roster.members.duplicate(),
                transport_entity: self.roster.transport_entity,
                cargo_entities: self.roster.cargo_entities.duplicate(),
                delivered_cargo: self.roster.delivered_cargo.duplicate(),
                // 複製は一覧へ戻されると別の提案として登録し直す。
                unsynced: true,
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
    pub(super) fn open(squad: &'a mut Squad) -> Self {
        // 未移行writerの書込みは、次に提案storeを読む時にこのSquadから取り込む。
        squad.roster.unsynced = true;
        Self {
            squad: &mut squad.roster,
        }
    }
}

impl RosterCommit<'_> {
    /// 通常メンバーへ追加する。追加された場合はtrue。
    pub(crate) fn join(&mut self, entity: Entity) -> bool {
        self.squad.members.0.insert(entity)
    }

    pub(crate) fn join_all(&mut self, entities: impl IntoIterator<Item = Entity>) {
        self.squad.members.0.extend(entities);
    }

    /// 通常メンバーから外す。外れた場合はtrue。積荷・輸送役bindingは変更しない。
    pub(crate) fn leave(&mut self, entity: Entity) -> bool {
        self.squad.members.0.remove(&entity)
    }

    pub(crate) fn retain_members(&mut self, keep: impl FnMut(&Entity) -> bool) {
        self.squad.members.0.retain(keep);
    }

    pub(crate) fn clear_members(&mut self) {
        self.squad.members.0.clear();
    }

    pub(crate) fn bind_transport(&mut self, transport: Option<Entity>) {
        self.squad.transport_entity.0 = transport;
    }

    /// 条件に合うEntityを通常メンバー・積荷・降車済み積荷から外し、
    /// 輸送役bindingも該当すれば外す（従来の3つのretain＋輸送役解除と同じ順序）。
    ///
    /// 述語は同じEntityに対して最大4回（members・cargo・delivered・transport）呼ばれる。
    /// 呼出し間で結果が変わる述語（状態を持つclosure等）を渡してはならない。
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

    pub(crate) fn clear_cargo(&mut self) {
        self.squad.cargo_entities.0.clear();
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

    pub(crate) fn retain_delivered(&mut self, keep: impl FnMut(&Entity) -> bool) {
        self.squad.delivered_cargo.0.retain(keep);
    }

    pub(crate) fn clear_delivered(&mut self) {
        self.squad.delivered_cargo.0.clear();
    }

    pub(crate) fn sort_delivered(&mut self) {
        self.squad
            .delivered_cargo
            .0
            .sort_by_key(|entity| entity.to_bits());
    }
}

/// テストの盤面組立てでは従来どおり直接書けるようにする。
/// 本番buildにはDerefMutが無く、所属の書込みはRosterCommitへ限定される。
#[cfg(test)]
mod fixture_access {
    use super::*;
    use std::ops::DerefMut;

    /// テストの盤面組立てだけは `squad.members.insert(..)` のように直接書ける。
    impl DerefMut for Squad {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.roster.unsynced = true;
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
/// 一覧の各Squadの非所属field（任務・目標・phase等）の更新は [`Self::squads_mut`] で行う。
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
    /// 手番途中の所属提案（正規化前の候補）。正規化の候補収集と明示的な提案viewはここを読む。
    proposals: ProposalStore,
}

/// 1つのSquadの所属提案（正規化前）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ProposedRoster {
    pub(crate) members: BTreeSet<Entity>,
    pub(crate) transport: Option<Entity>,
    pub(crate) cargo: Vec<Entity>,
    pub(crate) delivered: Vec<Entity>,
}

impl ProposedRoster {
    fn of(roster: &SquadRoster) -> Self {
        Self {
            members: roster.members.0.clone(),
            transport: roster.transport_entity.0,
            cargo: roster.cargo_entities.0.clone(),
            delivered: roster.delivered_cargo.0.clone(),
        }
    }

    fn entities(&self) -> impl Iterator<Item = (Entity, OperationUnitRole)> + '_ {
        self.members
            .iter()
            .map(|entity| {
                let role = if self.transport == Some(*entity) {
                    OperationUnitRole::Transport
                } else {
                    OperationUnitRole::Member
                };
                (*entity, role)
            })
            .chain(
                self.cargo
                    .iter()
                    .map(|entity| (*entity, OperationUnitRole::Cargo)),
            )
            .chain(
                self.delivered
                    .iter()
                    .map(|entity| (*entity, OperationUnitRole::DeliveredCargo)),
            )
    }
}

/// manager単位の所属提案store。SquadId→提案roster と Entity→候補（Squad・役割）の索引を持つ。
///
/// 移行中の同期規則: keyed確定口（[`ProposalCommit`]）はstoreとSquad側rosterへ同じ差分を
/// 同時に適用する。未移行writer（[`RosterCommit`]）・テストfixture・Squadの複製/新設は
/// Squadへ `unsynced` を立て、storeを読む前の [`SquadManager::sync_proposals`] がその
/// Squadだけをstoreへ取り込む（全writer移行後に廃止する逆算経路）。
#[derive(Debug, Default, Clone)]
pub(crate) struct ProposalStore {
    by_squad: HashMap<SquadId, ProposedRoster>,
    by_entity: HashMap<Entity, Vec<(SquadId, OperationUnitRole)>>,
}

impl ProposalStore {
    pub(crate) fn proposed(&self, squad_id: SquadId) -> Option<&ProposedRoster> {
        self.by_squad.get(&squad_id)
    }

    /// Entityを提案している（Squad・役割）。Squad一覧の順ではなく登録順。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn candidates(&self, entity: Entity) -> &[(SquadId, OperationUnitRole)] {
        self.by_entity.get(&entity).map_or(&[], Vec::as_slice)
    }

    fn unindex(&mut self, squad_id: SquadId) {
        let Some(old) = self.by_squad.remove(&squad_id) else {
            return;
        };
        self.remove_entity_refs(squad_id, &old);
    }

    fn remove_entity_refs(&mut self, squad_id: SquadId, old: &ProposedRoster) {
        for (entity, _) in old.entities() {
            if let Some(candidates) = self.by_entity.get_mut(&entity) {
                candidates.retain(|(id, _)| *id != squad_id);
                if candidates.is_empty() {
                    self.by_entity.remove(&entity);
                }
            }
        }
    }

    fn put(&mut self, squad_id: SquadId, roster: ProposedRoster) {
        self.unindex(squad_id);
        for (entity, role) in roster.entities() {
            self.by_entity
                .entry(entity)
                .or_default()
                .push((squad_id, role));
        }
        self.by_squad.insert(squad_id, roster);
    }

    /// 1件の提案を差分で更新する。索引は該当Squadの分だけ作り直す。
    fn update(&mut self, squad_id: SquadId, apply: impl FnOnce(&mut ProposedRoster)) {
        // 変更対象だけを所有権ごと取り出す。joinごとに集合・積荷列をcloneしない。
        let mut roster = self.by_squad.remove(&squad_id).unwrap_or_default();
        self.remove_entity_refs(squad_id, &roster);
        apply(&mut roster);
        self.put(squad_id, roster);
    }
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
    pub fn create_squad(&mut self, mission_type: MissionType) -> &mut Squad {
        let squad = Squad::new(SquadId(self.next_id), None, mission_type);
        self.next_id += 1;
        self.list.squads.push(squad);
        self.list.squads.last_mut().unwrap()
    }

    pub fn create_owned_squad(
        &mut self,
        mission_type: MissionType,
        owner_id: PlayerId,
    ) -> &mut Squad {
        let squad = self.create_squad(mission_type);
        squad.owner_id = Some(owner_id);
        squad
    }

    /// 各Squadの非所属fieldを更新するための可変slice（crate内限定）。所属fieldは読取り専用のまま。
    ///
    /// 型で防げないこと: `&mut [Squad]` は一般の可変sliceなので、`sort`・`swap`・`reverse` 等の
    /// 並べ替えや、要素への `*squad = 別のSquad値` によるSquad丸ごとの置換ができてしまう。
    /// 本番codeの一覧構造変更とSquad置換は [`SquadListCommit`]（`OperationLedger::squad_list`）
    /// だけで行う約束であり、典型形の直接置換が無いことはsource走査の契約テストで確認する。
    /// 走査は `squads_mut()[..] =` と `*squad =` の形だけを検出し、別名の可変参照経由の置換や
    /// slice並べ替えは検出できない（型保証ではない）。
    pub(crate) fn squads_mut(&mut self) -> &mut [Squad] {
        &mut self.list.squads
    }

    pub(super) fn list_commit(&mut self) -> SquadListCommit<'_> {
        SquadListCommit { manager: self }
    }

    /// 未反映のSquadを提案storeへ取り込み、一覧から消えたSquadの提案を外す。
    pub(crate) fn sync_proposals(&mut self) {
        let mut listed = HashSet::default();
        for squad in &mut self.list.squads {
            listed.insert(squad.id);
            if squad.roster.unsynced || !self.proposals.by_squad.contains_key(&squad.id) {
                self.proposals
                    .put(squad.id, ProposedRoster::of(&squad.roster));
                squad.roster.unsynced = false;
            }
        }
        let stale = self
            .proposals
            .by_squad
            .keys()
            .filter(|id| !listed.contains(id))
            .copied()
            .collect::<Vec<_>>();
        for squad_id in stale {
            self.proposals.unindex(squad_id);
        }
    }

    /// テスト用: keyed確定口を経由せずに書かれ、提案storeへ未反映のSquadがあるか。
    #[cfg(test)]
    pub(crate) fn has_unsynced_rosters(&self) -> bool {
        self.list
            .squads
            .iter()
            .any(|squad| squad.roster.unsynced || !self.proposals.by_squad.contains_key(&squad.id))
    }

    /// 直前に [`Self::sync_proposals`] 済みの提案view（`&self` で一覧と同時に読むため）。
    pub(crate) fn synced_proposals(&self) -> &ProposalStore {
        debug_assert!(
            self.list
                .squads
                .iter()
                .all(|squad| !squad.roster.unsynced
                    && self.proposals.by_squad.contains_key(&squad.id)),
            "提案storeが未同期"
        );
        &self.proposals
    }

    pub(super) fn proposal_commit(&mut self) -> ProposalCommit<'_> {
        self.sync_proposals();
        ProposalCommit { manager: self }
    }
}

/// SquadIdで指す所属提案の確定口。提案storeとSquad側rosterへ同じ差分を同時に適用する。
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
    ) -> &mut Squad {
        let roster = ProposedRoster {
            members,
            transport,
            cargo,
            delivered: Vec::new(),
        };
        let squad = self.manager.create_owned_squad(mission_type, owner_id);
        squad.roster.members.0 = roster.members.clone();
        squad.roster.transport_entity.0 = roster.transport;
        squad.roster.cargo_entities.0 = roster.cargo.clone();
        squad.roster.unsynced = false;
        let squad_id = squad.id;
        self.manager.proposals.put(squad_id, roster);
        let index = self.index_of(squad_id).expect("新設したSquad");
        &mut self.manager.list.squads[index]
    }

    /// 既存Squadへ通常メンバーとして提案する。追加された場合はtrue。
    pub(crate) fn join(&mut self, squad_id: SquadId, entity: Entity) -> bool {
        let Some(index) = self.index_of(squad_id) else {
            return false;
        };
        let squad = &mut self.manager.list.squads[index];
        let joined = squad.roster.members.0.insert(entity);
        if joined {
            self.manager.proposals.update(squad_id, |roster| {
                roster.members.insert(entity);
            });
        }
        joined
    }

    /// 配備接続を別Squadへ移すため、通常メンバーの提案を外す。
    pub(crate) fn leave(&mut self, squad_id: SquadId, entity: Entity) -> bool {
        let Some(index) = self.index_of(squad_id) else {
            return false;
        };
        let left = self.manager.list.squads[index]
            .roster
            .members
            .0
            .remove(&entity);
        if left {
            self.manager.proposals.update(squad_id, |roster| {
                roster.members.remove(&entity);
            });
        }
        left
    }

    /// 配備の受入集合を空にする。積荷・輸送役の提案は変更しない。
    pub(crate) fn clear_members(&mut self, squad_id: SquadId) {
        let Some(index) = self.index_of(squad_id) else {
            return;
        };
        self.manager.list.squads[index].roster.members.0.clear();
        self.manager
            .proposals
            .update(squad_id, |roster| roster.members.clear());
    }

    /// 責務の通常メンバーを置き換える。提案storeと実行viewへ同じ集合を適用する。
    pub(crate) fn replace_members(&mut self, squad_id: SquadId, members: BTreeSet<Entity>) {
        let Some(index) = self.index_of(squad_id) else {
            return;
        };
        self.manager.list.squads[index].roster.members.0 = members.clone();
        self.manager
            .proposals
            .update(squad_id, |roster| roster.members = members);
    }

    /// 責務移管の前に旧参照を外す。旧規則どおり輸送役binding自体は変更しない。
    pub(crate) fn remove_mentions(&mut self, squad_id: SquadId, entities: &HashSet<Entity>) {
        let Some(index) = self.index_of(squad_id) else {
            return;
        };
        let roster = &mut self.manager.list.squads[index].roster;
        roster.members.0.retain(|entity| !entities.contains(entity));
        roster
            .cargo_entities
            .0
            .retain(|entity| !entities.contains(entity));
        roster
            .delivered_cargo
            .0
            .retain(|entity| !entities.contains(entity));
        self.manager.proposals.update(squad_id, |roster| {
            roster.members.retain(|entity| !entities.contains(entity));
            roster.cargo.retain(|entity| !entities.contains(entity));
            roster.delivered.retain(|entity| !entities.contains(entity));
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
    pub(crate) fn squad_mut(&mut self, squad_id: SquadId) -> Option<&mut Squad> {
        let index = self.index_of(squad_id)?;
        Some(&mut self.manager.list.squads[index])
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

    /// 解散判定と同時に残すSquadを更新する（従来の `squads.retain_mut`）。
    /// 所属の書換えは閉包内でも `OperationLedger::roster` だけで行う。
    pub(crate) fn retain_mut(&mut self, keep: impl FnMut(&mut Squad) -> bool) {
        self.manager.list.squads.retain_mut(keep);
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

    /// 同じ位置のSquadを丸ごと置き換える（複製して更新したSquadの書戻し）。
    pub(crate) fn replace(&mut self, index: usize, squad: Squad) {
        self.manager.list.squads[index] = squad;
    }

    /// 発番済みSquadの末尾を一覧外へ取り出す（新規idだけを使う再構成用）。
    pub(crate) fn pop(&mut self) -> Option<Squad> {
        self.manager.list.squads.pop()
    }

    /// SquadId順へ並べ直す。
    pub(crate) fn sort_by_id(&mut self) {
        self.manager.list.squads.sort_by_key(|squad| squad.id.0);
    }

    /// 一覧外で組んだSquadへ発番順にidを与えて追加する（輸送波の手動発番）。
    pub(crate) fn push_numbered(&mut self, mut squad: Squad) {
        squad.id = SquadId(self.manager.next_id);
        self.manager.next_id = self.manager.next_id.saturating_add(1);
        self.manager.list.squads.push(squad);
    }
}

/// テストの盤面組立てだけは一覧を直接書ける。
#[cfg(test)]
impl std::ops::DerefMut for SquadManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.list
    }
}
