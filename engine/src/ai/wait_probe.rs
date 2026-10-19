//! Waitの選択・発行を観測する診断専用プローブ。AIの候補・順位・scoreを変更しない。
//!
//! 既定OFF。記録は決定器の実評価中に集め、最終的に発行したWaitだけをMCPへ渡す。
//! このResourceの内容はゲーム状態・所属・命令の判断には使用しない。

use crate::ai::command::AiCommand;
use crate::ai::tactics::board::TacticalBoard;
use crate::ai::tactics::campaign::CampaignActionContext;
use crate::ai::tactics::scoring::Rank;
use crate::ai::tactics::unit::UnitState;
use crate::collections::HashMap;
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, Transporting,
    UnitStats,
};
use crate::resources::{MatchState, UnitType};
use bevy_ecs::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

const SAMPLES_PER_REASON: usize = 3;

/// 診断の出力量だけを制御する。戦術の対象unitや候補を絞る設定ではない。
#[derive(Debug, Clone, Default)]
pub struct WaitProbeSettings {
    pub enabled: bool,
    pub players: HashSet<u32>,
    pub units: HashSet<u64>,
    pub unit_types: HashSet<String>,
    pub turns: Option<(u32, u32)>,
}

impl WaitProbeSettings {
    pub fn from_env() -> Self {
        let enabled = std::env::var("OPENWARS_WAIT_PROBE")
            .is_ok_and(|value| !value.is_empty() && value != "0");
        if !enabled {
            return Self::default();
        }
        let split = |key: &str| {
            std::env::var(key)
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let turn_filter = std::env::var("OPENWARS_WAIT_PROBE_TURNS").unwrap_or_default();
        let turns = parse_turns(&turn_filter);
        let players = parse_ids(split("OPENWARS_WAIT_PROBE_PLAYERS"));
        let units = parse_ids(split("OPENWARS_WAIT_PROBE_UNITS"));
        // 不正な絞込を「指定なし」と解釈して全件出力へ広げない。
        let (Some(players), Some(units)) = (players, units) else {
            return Self::default();
        };
        if !turn_filter.trim().is_empty() && turns.is_none() {
            return Self::default();
        }
        Self {
            enabled,
            players,
            units,
            unit_types: split("OPENWARS_WAIT_PROBE_TYPES")
                .into_iter()
                .map(|name| {
                    UnitType::from_english_str(&name)
                        .map_or(name.clone(), |kind| kind.as_str().to_owned())
                })
                .collect(),
            turns,
        }
    }

    fn at_turn(&self, player: PlayerId, turn: u32) -> bool {
        self.enabled
            && (self.players.is_empty() || self.players.contains(&player.0))
            && self
                .turns
                .is_none_or(|(first, last)| first <= turn && turn <= last)
    }

    fn details(&self, entity: Entity, unit_type: UnitType) -> bool {
        (self.units.is_empty() || self.units.contains(&entity.to_bits()))
            && (self.unit_types.is_empty() || self.unit_types.contains(unit_type.as_str()))
    }
}

fn parse_ids<T: std::str::FromStr + Eq + std::hash::Hash>(
    values: Vec<String>,
) -> Option<HashSet<T>> {
    values.into_iter().map(|value| value.parse().ok()).collect()
}

fn parse_turns(value: &str) -> Option<(u32, u32)> {
    let (first, last) = value
        .trim()
        .split_once('-')
        .or_else(|| value.trim().split_once(':'))?;
    let first = first.trim().parse().ok()?;
    let last = last.trim().parse().ok()?;
    (first <= last).then_some((first, last))
}

/// 実評価から取得したscore項目。既存の加算を再実行せず中間値だけを保持する。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct ScoreParts {
    pub terrain: i32,
    pub positioning: i32,
    pub squad_approach: i32,
    pub solo_approach: i32,
    pub transport_return: i32,
    pub stranded: i32,
    pub capture_or_hunt: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct Offer {
    pub unit: Entity,
    pub kind: &'static str,
    pub position: GridPosition,
    pub target: Option<Entity>,
    pub rank: Rank,
    pub parts: Option<ScoreParts>,
    pub action_bonus: i32,
}

impl Offer {
    fn value(&self) -> Value {
        json!({"unit_id":self.unit.to_bits(), "action":self.kind,
            "position":[self.position.x,self.position.y], "target_id":self.target.map(Entity::to_bits),
            "priority":format!("{:?}",self.rank.0), "priority_rank":self.rank.0 as u8,
            "score":self.rank.1, "score_parts":self.parts, "action_bonus":self.action_bonus})
    }
}

/// 1unitの実評価。詳細を絞った場合も全体の非Wait代案に必要なofferは追跡する。
pub(crate) struct UnitProbe {
    pub detailed: bool,
    pub context: Value,
    pub best_non_wait: Option<Offer>,
    best_by_action: BTreeMap<&'static str, Offer>,
    best_moving_wait: Option<Offer>,
    offered: BTreeMap<&'static str, u64>,
    rejected: BTreeMap<&'static str, u64>,
    samples: BTreeMap<&'static str, Vec<Value>>,
    generated_tiles: usize,
}

impl UnitProbe {
    fn new(detailed: bool) -> Self {
        Self {
            detailed,
            context: Value::Null,
            best_non_wait: None,
            best_by_action: BTreeMap::new(),
            best_moving_wait: None,
            offered: BTreeMap::new(),
            rejected: BTreeMap::new(),
            samples: BTreeMap::new(),
            generated_tiles: 0,
        }
    }

    pub(crate) fn tiles(&mut self, count: usize) {
        if self.detailed {
            self.generated_tiles = count;
        }
    }

    /// JSONの構築も詳細ON時だけ。棄却数はすべて数え、代表例だけを上限付きで保存する。
    pub(crate) fn reject(
        &mut self,
        reason: &'static str,
        position: GridPosition,
        facts: impl FnOnce() -> Value,
    ) {
        if !self.detailed {
            return;
        }
        *self.rejected.entry(reason).or_default() += 1;
        let samples = self.samples.entry(reason).or_default();
        if samples.len() < SAMPLES_PER_REASON {
            samples.push(json!({"position":[position.x,position.y],"facts":facts()}));
        }
    }

    pub(crate) fn offer(&mut self, offer: Offer) {
        if offer.kind != "Wait"
            && self
                .best_non_wait
                .as_ref()
                .is_none_or(|old| offer.rank > old.rank)
        {
            self.best_non_wait = Some(offer.clone());
        }
        if !self.detailed {
            return;
        }
        *self.offered.entry(offer.kind).or_default() += 1;
        if offer.kind == "Wait"
            && self.context["position"] != json!([offer.position.x, offer.position.y])
            && self
                .best_moving_wait
                .as_ref()
                .is_none_or(|old| offer.rank > old.rank)
        {
            self.best_moving_wait = Some(offer.clone());
        }
        if self
            .best_by_action
            .get(offer.kind)
            .is_none_or(|old| offer.rank > old.rank)
        {
            self.best_by_action.insert(offer.kind, offer);
        }
    }

    fn value(&self) -> Value {
        json!({"context":self.context,"generated_candidate_tiles":self.generated_tiles,
            "offered_counts":self.offered,"rejected_counts":self.rejected,"rejection_samples":self.samples,
            "best_by_action":self.best_by_action.iter().map(|(k,v)|(*k,v.value())).collect::<BTreeMap<_,_>>(),
            "best_moving_wait":self.best_moving_wait.as_ref().map(Offer::value),
            "counting_units":"weapon rejection counts are per tested target/weapon/tile; later gates may be unevaluated"})
    }
}

/// 1回の通常戦術の観測。すべてのunitの非Wait offerを比較し、詳細だけをフィルタする。
pub(crate) struct ProbeCall {
    settings: WaitProbeSettings,
    call_id: u64,
    selector: u64,
    player: PlayerId,
    turn: u32,
    units: HashMap<Entity, UnitProbe>,
    best_non_wait: Option<Offer>,
    evaluated_units: usize,
}

impl ProbeCall {
    pub(crate) fn start(world: &mut World, player: PlayerId) -> Option<Self> {
        let turn = world
            .get_resource::<MatchState>()
            .map_or(0, |s| s.current_turn_number.0);
        let probe = world.get_resource::<WaitProbe>()?;
        if !probe.settings.at_turn(player, turn)
            || crate::ai::resolve_player_ai_version(world, player) != crate::ai::AiVersion::V4
        {
            return None;
        }
        let mut probe = world.resource_mut::<WaitProbe>();
        probe.selector += 1;
        probe.pending = None;
        Some(Self {
            settings: probe.settings.clone(),
            call_id: probe.call_id,
            selector: probe.selector,
            player,
            turn,
            units: HashMap::default(),
            best_non_wait: None,
            evaluated_units: 0,
        })
    }

    pub(crate) fn unit(&self, entity: Entity, unit_type: UnitType) -> UnitProbe {
        UnitProbe::new(self.settings.details(entity, unit_type))
    }

    pub(crate) fn add_unit(&mut self, entity: Entity, unit: UnitProbe) {
        self.evaluated_units += 1;
        if let Some(offer) = &unit.best_non_wait
            && self
                .best_non_wait
                .as_ref()
                .is_none_or(|old| offer.rank > old.rank)
        {
            self.best_non_wait = Some(offer.clone());
        }
        if unit.detailed {
            self.units.insert(entity, unit);
        }
    }

    pub(crate) fn finish(self, world: &mut World, entity: Entity, command: &AiCommand, rank: Rank) {
        let AiCommand::Wait { target_pos } = command else {
            return;
        };
        let Some(unit) = self.units.get(&entity) else {
            return;
        };
        let comparison = self.best_non_wait.as_ref().map(|other| {
            let reason = if rank.0 > other.rank.0 {
                "higher_priority"
            } else if rank.1 > other.rank.1 {
                "higher_score"
            } else if rank == other.rank {
                "tie_keep_first"
            } else {
                "inconsistent_probe"
            };
            json!({"reason":reason,"alternative":other.value()})
        });
        let value = json!({"schema_version":1,"decision_call_id":self.call_id,"selector_ordinal":self.selector,
            "evaluation_player_id":self.player.0,"evaluation_turn":self.turn,
            "selection_stage":"standard_tactics","unit_id":entity.to_bits(),"requested_position":[target_pos.x,target_pos.y],
            "selected":{"priority":format!("{:?}",rank.0),"priority_rank":rank.0 as u8,"score":rank.1},
            "evaluation":unit.value(),"global_best_non_wait":comparison,
            "coverage":{"evaluated_units":self.evaluated_units,"non_wait_offers":"all evaluated units, before unit-best pruning",
                "detail_units":self.units.len(),"unrecorded_departure_reason":"departure flag origin is not persisted; not inferred"}});
        if let Some(mut probe) = world.get_resource_mut::<WaitProbe>()
            && probe.call_id == self.call_id
            && probe.selector == self.selector
        {
            probe.pending = Some((entity, *target_pos, value));
        }
    }
}

/// 診断専用の一時バッファ。発行しなかった選択結果を次のstepへ持ち越さない。
#[derive(Resource)]
pub struct WaitProbe {
    pub settings: WaitProbeSettings,
    call_id: u64,
    selector: u64,
    stage: &'static str,
    pending: Option<(Entity, GridPosition, Value)>,
    issued: Option<Value>,
}

impl Default for WaitProbe {
    fn default() -> Self {
        Self {
            settings: WaitProbeSettings::from_env(),
            call_id: 0,
            selector: 0,
            stage: "untraced",
            pending: None,
            issued: None,
        }
    }
}

pub(crate) fn begin_step(world: &mut World) {
    // 復元World等でも同じ診断口を用意する。ON/OFFで登録有無を分けない。
    world.init_resource::<WaitProbe>();
    if let Some(mut p) = world.get_resource_mut::<WaitProbe>() {
        // OFF時も以前の診断を流用しない。ゲームの状態は一切変更しない。
        p.pending = None;
        p.issued = None;
        if p.settings.enabled {
            p.call_id += 1;
            p.selector = 0;
            p.stage = "untraced";
        }
    }
}

pub(crate) fn stage(world: &mut World, name: &'static str) {
    if let Some(mut p) = world.get_resource_mut::<WaitProbe>()
        && p.settings.enabled
    {
        p.stage = name;
        p.pending = None;
    }
}

/// 命令の最終発行口でのみ確定。別段の同じWaitへ古い評価を誤結合しない。
pub(crate) fn issued(world: &mut World, entity: Entity, command: &AiCommand) {
    // 非Waitやフィルタ外の命令でも以前の選択結果を消費し、次の発行へ持ち越さない。
    let pending = {
        let Some(mut probe) = world.get_resource_mut::<WaitProbe>() else {
            return;
        };
        probe.issued = None;
        let pending = probe.pending.take();
        if !probe.settings.enabled {
            return;
        }
        pending
    };
    let AiCommand::Wait { target_pos } = command else {
        return;
    };
    let Some(faction) = world.get::<Faction>(entity).copied() else {
        return;
    };
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |s| s.current_turn_number.0);
    let Some(stats) = world.get::<UnitStats>(entity) else {
        return;
    };
    let Some(p) = world.get_resource::<WaitProbe>() else {
        return;
    };
    if !p.settings.at_turn(faction.0, turn)
        || !p.settings.details(entity, stats.unit_type)
        || crate::ai::resolve_player_ai_version(world, faction.0) != crate::ai::AiVersion::V4
    {
        return;
    }
    let origin = world.get::<GridPosition>(entity).copied();
    let binding = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .and_then(|r| r.assignment(entity));
    let squad = binding.and_then(|b| b.squad_id).and_then(|id| {
        world
            .get_resource::<crate::ai::squad::SquadManager>()
            .and_then(|m| m.squads.iter().find(|s| s.id == id))
    });
    let issue_context = json!({"unit_type":stats.unit_type.as_str(),"can_capture":stats.can_capture,
        "hp":world.get::<Health>(entity).map(|v|v.current),
        "fuel":world.get::<crate::components::Fuel>(entity).map(|v|v.current),
        "ammo":world.get::<crate::components::Ammo>(entity).map(|v|[v.ammo1,v.ammo2]),
        "has_moved":world.get::<HasMoved>(entity).map(|v|v.0),
        "action_completed":world.get::<ActionCompleted>(entity).map(|v|v.0),
        "squad_id":binding.and_then(|b|b.squad_id).map(|id|id.0),
        "mission":squad.map(|s|format!("{:?}",s.plan.mission_type)),
        "phase":squad.map(|s|format!("{:?}",s.plan.phase)),
        "squad_target":squad.and_then(|s|s.plan.target).map(|p|[p.x,p.y]),
        "departure_authorized":squad.map(|s|s.plan.departure_authorized)});
    let mut p = world.resource_mut::<WaitProbe>();
    let mut value = match pending {
        Some((selected, destination, value))
            if selected == entity
                && destination == *target_pos
                && value["evaluation_player_id"] == json!(faction.0.0)
                && value["evaluation_turn"] == json!(turn)
                && value["decision_call_id"] == json!(p.call_id)
                && value["selector_ordinal"] == json!(p.selector) =>
        {
            value
        }
        _ => json!({"schema_version":1,"decision_call_id":p.call_id,"unit_id":entity.to_bits(),
            "selection_stage":"untraced","coverage":{"reason":"issued outside the observed standard-tactics choice"}}),
    };
    value["issuance_context"] = issue_context;
    value["runtime_stage"] = json!(p.stage);
    value["player_id"] = json!(faction.0.0);
    value["turn"] = json!(turn);
    value["origin"] = json!(origin.map(|pos| [pos.x, pos.y]));
    value["requested_position"] = json!([target_pos.x, target_pos.y]);
    value["intended_move"] = json!(origin.is_some_and(|pos| pos != *target_pos));
    p.issued = Some(value);
}

/// MCPはschedule適用後に一度だけ取得し、実位置を付記する。
pub fn take_issued(world: &mut World) -> Option<Value> {
    world.get_resource_mut::<WaitProbe>()?.issued.take()
}

fn goal_facts(world: &World, board: &TacticalBoard, goal: GridPosition) -> Value {
    let property = world.iter_entities().find_map(|e| {
        (e.get::<GridPosition>() == Some(&goal))
            .then(|| e.get::<Property>().copied())
            .flatten()
    });
    let occupants=world.iter_entities().filter(|e|e.get::<GridPosition>()==Some(&goal) && e.get::<Transporting>().is_none())
        .filter_map(|e| Some(json!({"unit_id":e.id().to_bits(),"unit_type":e.get::<UnitStats>()?.unit_type.as_str(),
            "player_id":e.get::<Faction>()?.0.0,"can_capture":e.get::<UnitStats>()?.can_capture}))).collect::<Vec<_>>();
    let registry = world.get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>();
    let claimants=board.squads.manager.squads.iter().filter(|s|s.plan.owner_id==Some(board.player_id)
        && s.plan.mission_type==crate::ai::squad::MissionType::Capture && s.plan.target==Some(goal))
        .flat_map(|s|s.members.iter().map(move |e|(s.id,*e)))
        .filter(|(id,e)|registry.is_none_or(|r|r.assignment(*e).is_some_and(|b|b.squad_id==Some(*id))))
        .filter_map(|(id,e)|Some(json!({"unit_id":e.to_bits(),"squad_id":id.0,
            "position":world.get::<GridPosition>(e).map(|p|[p.x,p.y]),"can_capture":world.get::<UnitStats>(e)?.can_capture,
            "hp":world.get::<Health>(e).map(|h|h.current)}))).collect::<Vec<_>>();
    json!({"position":[goal.x,goal.y],"is_property":property.is_some(),
        "owner":property.and_then(|p|p.owner_id).map(|p|p.0),"capture_points":property.map(|p|p.capture_points),
        "occupants":occupants,"capture_claimants":claimants,"claimant_binding_checked":registry.is_some()})
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unit_context(
    world: &World,
    board: &TacticalBoard,
    state: &UnitState,
    squad_target: Option<GridPosition>,
    campaign: Option<&CampaignActionContext>,
    deployment: Option<Entity>,
    is_solo: bool,
) -> Value {
    let binding = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .and_then(|r| r.assignment(state.entity));
    let squad = binding
        .and_then(|b| b.squad_id)
        .and_then(|id| board.squads.manager.squads.iter().find(|s| s.id == id));
    json!({"unit_id":state.entity.to_bits(),"unit_type":state.stats.unit_type.as_str(),"position":[state.position.x,state.position.y],
        "hp":state.hp,"fuel":state.fuel,"ammo":[state.ammo.0,state.ammo.1],
        "movement_type":format!("{:?}",state.stats.movement_type),"max_movement":state.stats.max_movement,
        "min_range":state.stats.min_range,"max_range":state.stats.max_range,"can_capture":state.stats.can_capture,
        "has_moved":world.get::<HasMoved>(state.entity).map(|v|v.0),"action_completed":world.get::<ActionCompleted>(state.entity).map(|v|v.0),
        "combat_ineffective":state.combat_ineffective,"is_solo":is_solo,"deployment_target":deployment.map(Entity::to_bits),
        "binding":binding.map(|b|json!({"owner":format!("{:?}",b.owner),"squad_id":b.squad_id.map(|id|id.0),"role":format!("{:?}",b.role)})),
        "mission":squad.map(|s|format!("{:?}",s.plan.mission_type)),"phase":squad.map(|s|format!("{:?}",s.plan.phase)),
        "departure_authorized":campaign.map(|c|c.departure_authorized),
        "squad_target":squad_target.map(|p|[p.x,p.y]),"campaign_target":campaign.and_then(|c|c.target).map(|p|[p.x,p.y]),
        "squad_goal":squad_target.map(|p|goal_facts(world,board,p)),
        "campaign_goal":campaign.and_then(|c|c.target).map(|p|goal_facts(world,board,p))})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::tactics::scoring::ActionPriority;

    #[test]
    fn probe_invalid_filters_do_not_become_unrestricted() {
        assert_eq!(parse_ids::<u32>(vec![]), Some(HashSet::new()));
        assert_eq!(parse_ids::<u32>(vec!["2".into(), "bad".into()]), None);
        assert_eq!(parse_ids::<u64>(vec!["bad".into()]), None);
        assert_eq!(parse_turns("25-50"), Some((25, 50)));
        assert_eq!(parse_turns("25:50"), Some((25, 50)));
        assert_eq!(parse_turns("50-25"), None);
        assert_eq!(parse_turns("25-bad"), None);
    }

    #[test]
    fn probe_keeps_first_equal_offer_and_tracks_non_wait_losing_to_wait() {
        let unit = Entity::from_raw(1);
        let mut probe = UnitProbe::new(true);
        probe.context = json!({"position":[0,0]});
        let offer = |kind, pos, score| Offer {
            unit,
            kind,
            position: pos,
            target: None,
            rank: (ActionPriority::Normal, score),
            parts: None,
            action_bonus: 0,
        };
        probe.offer(offer("Attack", GridPosition { x: 1, y: 0 }, 10));
        probe.offer(offer("Attack", GridPosition { x: 2, y: 0 }, 10));
        probe.offer(offer("Wait", GridPosition { x: 0, y: 0 }, 20));
        assert_eq!(
            probe.best_non_wait.as_ref().unwrap().position,
            GridPosition { x: 1, y: 0 }
        );
        assert_eq!(probe.best_non_wait.as_ref().unwrap().rank.1, 10);
        assert_eq!(probe.best_by_action["Wait"].rank.1, 20);
    }

    #[test]
    fn observing_offers_never_calls_a_losing_command_closure() {
        use crate::ai::tactics::scoring::BestAction;
        use std::cell::Cell;
        let mut outcomes = Vec::new();
        for enabled in [false, true] {
            let mut probe = enabled.then(|| UnitProbe::new(true));
            let mut best = BestAction::default();
            let calls = Cell::new(0);
            for (index, score) in [10, 10, 5, 20].into_iter().enumerate() {
                let position = GridPosition { x: index, y: 0 };
                if let Some(trace) = probe.as_mut() {
                    trace.offer(Offer {
                        unit: Entity::from_raw(1),
                        kind: "Wait",
                        position,
                        target: None,
                        rank: (ActionPriority::Normal, score),
                        parts: None,
                        action_bonus: 0,
                    });
                }
                best.offer((ActionPriority::Normal, score), || {
                    calls.set(calls.get() + 1);
                    AiCommand::Wait {
                        target_pos: position,
                    }
                });
            }
            outcomes.push((calls.get(), format!("{:?}", best.command())));
        }
        assert_eq!(outcomes[0], outcomes[1]);
        assert_eq!(outcomes[0].0, 2, "同点と敗者のクロージャを実行しない");
    }

    #[test]
    fn filtered_details_do_not_remove_global_non_wait_offers() {
        let mut probe = UnitProbe::new(false);
        probe.offer(Offer {
            unit: Entity::from_raw(1),
            kind: "Capture",
            position: GridPosition { x: 1, y: 0 },
            target: None,
            rank: (ActionPriority::Normal, 100),
            parts: None,
            action_bonus: 0,
        });
        assert!(probe.best_non_wait.is_some());
        assert!(probe.best_by_action.is_empty());
        probe.reject("unused", GridPosition { x: 0, y: 0 }, || {
            panic!("詳細OFFで生成しない")
        });
    }
}
