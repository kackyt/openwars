//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

pub(super) fn reserve_candidate(
    candidate: &IslandCampaignCandidate,
    pool: &CampaignResourcePool,
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
    allow_future_budget_reservation: bool,
) -> Option<(IslandCampaignAssignment, CampaignResourcePool)> {
    // 候補ごとのcloneへ全予約を適用し、完全編成を賄えない場合は元poolを一切変更しない。
    let mut provisional = pool.clone();
    let island_id = candidate.assessment.island_id;
    let requirement = &candidate.requirement;
    let mut transport_entities = Vec::new();
    let mut capture_entities = Vec::new();
    let mut combat_entities = Vec::new();
    let mut reserved_entity_value = 0_u32;
    let mut requirement_credit = 0_u32;
    let mut remote_transport_demand = None;
    let existing = candidate.existing_operation.as_ref();

    if let Some(operation) = existing {
        if operation.is_forming || is_live_campaign_transport_phase(operation.transport_phase) {
            for entity in &operation.transport_entities {
                if let Some(unit) = catalog.get(entity)
                    && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                {
                    push_unique_entity(&mut transport_entities, *entity);
                    reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                    remove_entity(&mut provisional, *entity);
                }
            }
        } else {
            // Return/Completed輸送は同じ島への再予約候補としても扱わない。
            for entity in &operation.transport_entities {
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.capture_entities {
            if let Some(unit) = catalog.get(entity)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut capture_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.combat_entities {
            if let Some(unit) = catalog.get(entity)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut combat_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
    }
    let continued_from_existing_squad = !transport_entities.is_empty()
        || !capture_entities.is_empty()
        || !combat_entities.is_empty();

    if candidate.assessment.decision == IslandCampaignDecision::Contest {
        let local_assets: Vec<_> = sorted_pool_units(&provisional, island_id)
            .into_iter()
            .filter(|unit| {
                unit.island_id == Some(island_id) && !is_campaign_support_unit(unit.unit_type)
            })
            .collect();
        for unit in local_assets {
            if unit.can_capture {
                push_unique_entity(&mut capture_entities, unit.entity);
            } else {
                push_unique_entity(&mut combat_entities, unit.entity);
            }
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
        }
    }

    let mut remaining_transport_slots = requirement.transport_slots;
    let mut missing_assault_transports = candidate.assault_transport_types.clone();
    let mut missing_helicopter = candidate.assessment.decision == IslandCampaignDecision::Expand;
    for entity in &transport_entities {
        let Some(unit) = catalog.get(entity) else {
            continue;
        };
        if let Some(index) = missing_assault_transports
            .iter()
            .position(|required| *required == unit.unit_type)
        {
            missing_assault_transports.remove(index);
        }
        if candidate.assessment.decision == IslandCampaignDecision::Expand
            && unit.unit_type == UnitType::TransportHelicopter
        {
            missing_helicopter = false;
        }
        let satisfies_transport_requirement = match candidate.assessment.decision {
            IslandCampaignDecision::Expand => unit.unit_type == UnitType::TransportHelicopter,
            _ => is_offshore_transport(unit.unit_type),
        };
        if satisfies_transport_requirement {
            let credited_slots = remaining_transport_slots.min(unit.available_cargo_slots);
            remaining_transport_slots = remaining_transport_slots.saturating_sub(credited_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }

    let mut available = sorted_pool_units(&provisional, island_id);
    if candidate.assessment.decision == IslandCampaignDecision::Assault {
        let required_types = missing_assault_transports.clone();
        for required_type in required_types {
            if let Some(index) = available.iter().position(|unit| {
                unit.unit_type == required_type && is_offshore_transport(unit.unit_type)
            }) {
                let unit = available.remove(index);
                push_unique_entity(&mut transport_entities, unit.entity);
                remaining_transport_slots =
                    remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
                requirement_credit = requirement_credit.saturating_add(unit.cost);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, unit.entity);
                if let Some(index) = missing_assault_transports
                    .iter()
                    .position(|missing| *missing == required_type)
                {
                    missing_assault_transports.remove(index);
                }
            }
        }
    } else {
        while remaining_transport_slots > 0 {
            let Some(index) = available.iter().position(|unit| {
                is_offshore_transport(unit.unit_type)
                    && requirement
                        .preferred_transport
                        .is_none_or(|preferred| unit.unit_type == preferred)
            }) else {
                break;
            };
            let unit = available.remove(index);
            push_unique_entity(&mut transport_entities, unit.entity);
            remaining_transport_slots =
                remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
            if unit.unit_type == UnitType::TransportHelicopter {
                missing_helicopter = false;
            }
        }
    }

    let mut remaining_capture_units = requirement.capture_units;
    let mut capture_entities_used_for_capture = 0_usize;
    for entity in &capture_entities {
        if remaining_capture_units == 0 {
            break;
        }
        if let Some(unit) = catalog.get(entity) {
            remaining_capture_units = remaining_capture_units.saturating_sub(1);
            capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }
    available = sorted_pool_units(&provisional, island_id);
    // 【一時的な診断専用】占領役の補充直前に、provisional poolの全Entityと
    // `sorted_pool_units` の assigned_island フィルタ結果を突き合わせる。
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        eprintln!(
            "POOL_AUDIT stage=capture_fill_begin island={:?} decision={:?} required_capture_units={} already_reserved={:?} remaining={} provisional_total={} after_assigned_filter={}",
            island_id,
            candidate.assessment.decision,
            requirement.capture_units,
            capture_entities,
            remaining_capture_units,
            provisional.units.len(),
            available.len(),
        );
        for unit in &provisional.units {
            let passes_assigned_filter =
                unit.assigned_island.is_none() || unit.assigned_island == Some(island_id);
            eprintln!(
                "POOL_AUDIT stage=capture_fill_unit island={:?} entity={:?} type={:?} can_capture={} unit_island={:?} assigned_island={:?} passes_assigned_filter={}",
                island_id,
                unit.entity,
                unit.unit_type,
                unit.can_capture,
                unit.island_id,
                unit.assigned_island,
                passes_assigned_filter,
            );
        }
    }
    while remaining_capture_units > 0 {
        let Some(index) = available.iter().position(|unit| unit.can_capture) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut capture_entities, unit.entity);
        remaining_capture_units = remaining_capture_units.saturating_sub(1);
        capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
        requirement_credit = requirement_credit.saturating_add(unit.cost);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }

    // `ground_combat_units` は従来の渡洋Assaultの台数見積りには使わない。同一陸塊で
    // 地形分断を越える継続campaignだけが、分析済みの橋突破不足として保持する。
    let maintains_armored_route_campaign = matches!(
        candidate.assessment.decision,
        IslandCampaignDecision::Secure
            | IslandCampaignDecision::Contest
            | IslandCampaignDecision::Reinforce
    ) && requirement.ground_combat_units > 0;
    let mut remaining_ground_combat_units = if maintains_armored_route_campaign {
        requirement.ground_combat_units
    } else {
        0
    };
    for entity in &combat_entities {
        if remaining_ground_combat_units == 0 {
            break;
        }
        if catalog
            .get(entity)
            .is_some_and(|unit| is_route_armored_unit(unit.unit_type))
        {
            remaining_ground_combat_units = remaining_ground_combat_units.saturating_sub(1);
            // 橋突破に必要な装甲枠は、既存の実Entityで満たした場合だけ構造予算から控除する。
            requirement_credit =
                requirement_credit.saturating_add(catalog.get(entity).map_or(0, |unit| unit.cost));
        }
    }
    available = sorted_pool_units(&provisional, island_id);
    while remaining_ground_combat_units > 0 {
        let Some(index) = available.iter().position(|unit| {
            is_route_armored_unit(unit.unit_type)
                && (unit.island_id == Some(island_id)
                    || unit
                        .reachable_positions
                        .contains(&candidate.target_position))
        }) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut combat_entities, unit.entity);
        remaining_ground_combat_units = remaining_ground_combat_units.saturating_sub(1);
        requirement_credit = requirement_credit.saturating_add(unit.cost);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }
    // 現在手番に橋突破を要する同一陸塊campaignでは、未予約の装甲unitも同じ
    // campaignへ回収する。必要数のcreditには入れないので、単なる予算相殺にはしない。
    if maintains_armored_route_campaign {
        available = sorted_pool_units(&provisional, island_id);
        let attached_armor = available
            .iter()
            .filter(|unit| {
                is_route_armored_unit(unit.unit_type)
                    && (unit.island_id == Some(island_id)
                        || unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            })
            .cloned()
            .collect::<Vec<_>>();
        for unit in attached_armor {
            push_unique_entity(&mut combat_entities, unit.entity);
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
        }
    }

    let mut remaining_combat_units = requirement.combat_units;
    for entity in capture_entities
        .iter()
        .skip(capture_entities_used_for_capture)
        .chain(combat_entities.iter())
    {
        if remaining_combat_units == 0 {
            break;
        }
        if catalog.contains_key(entity) {
            // capture枠へ使わなかった実Entityを、戦闘参加者として一体だけ充当する。
            remaining_combat_units = remaining_combat_units.saturating_sub(1);
            // 戦闘Entityの価格で輸送・占領の構造予算を相殺してはならない。
        }
    }
    let assault_wave_is_frozen = candidate.assessment.decision == IslandCampaignDecision::Assault
        && existing
            .is_some_and(|operation| is_live_campaign_transport_phase(operation.transport_phase));
    available = sorted_pool_units(&provisional, island_id);
    // Combat候補ごとにpool全体を再走査しない。既に選択済みと、この段階で予約可能な
    // 輸送役を一度だけまとめ、各cargo追加時の二部マッチングへ再利用する。
    let mut route_transports = transport_entities.clone();
    for transport in &available {
        if is_offshore_transport(transport.unit_type) {
            push_unique_entity(&mut route_transports, transport.entity);
        }
    }
    while remaining_combat_units > 0 && !assault_wave_is_frozen {
        let Some(index) = available.iter().position(|unit| {
            if is_campaign_support_unit(unit.unit_type) {
                return false;
            }
            if candidate.assessment.decision == IslandCampaignDecision::Defend {
                return !unit.is_transporting
                    && unit
                        .reachable_positions
                        .contains(&candidate.target_position);
            }
            if unit.island_id == Some(island_id)
                || unit
                    .reachable_positions
                    .contains(&candidate.target_position)
            {
                return true;
            }

            // 自力展開できない渡洋支援は、現在の輸送役または生産可能な輸送手段で
            // 実際に運べる場合だけ選ぶ。Contest/Reinforceを無条件で通すと、戦車など
            // 積載不能Entityが完全編成へ混ざり、後段で島作戦全体が消える。
            let mut remote_cargo: Vec<_> = capture_entities
                .iter()
                .chain(combat_entities.iter())
                .copied()
                .filter(|entity| {
                    catalog.get(entity).is_some_and(|cargo| {
                        cargo.island_id != Some(island_id)
                            && !cargo
                                .reachable_positions
                                .contains(&candidate.target_position)
                    })
                })
                .collect();
            remote_cargo.push(unit.entity);
            // transport_slotsが0のReinforceでも、pool内に完成済み輸送役があれば
            // 後段でその実Entityを予約できる。先に選択済みの輸送役だけを見ると、
            // 合法なLander付き編成まで「輸送経路なし」として落としてしまう。
            campaign_transport_package_covers(&remote_cargo, &route_transports, catalog)
                || remote_transport_shortfall(
                    &remote_cargo,
                    &route_transports,
                    catalog,
                    &candidate.producible_transports,
                )
                .is_some()
        }) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut combat_entities, unit.entity);
        remaining_combat_units = remaining_combat_units.saturating_sub(1);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }
    if matches!(
        candidate.assessment.decision,
        IslandCampaignDecision::Expand
            | IslandCampaignDecision::Secure
            | IslandCampaignDecision::Contest
            | IslandCampaignDecision::Reinforce
    ) {
        let remote_cargo: Vec<_> = capture_entities
            .iter()
            .chain(combat_entities.iter())
            .copied()
            .filter(|entity| {
                catalog.get(entity).is_some_and(|unit| {
                    !unit.is_transporting
                        && unit
                            .island_id
                            .is_some_and(|source_island| source_island != island_id)
                        && !unit
                            .reachable_positions
                            .contains(&candidate.target_position)
                })
            })
            .collect();
        if !remote_cargo.is_empty() {
            // 後続波も総輸送枠だけでreadyにせず、cargoと同じ出発島から実搭載できることを確認する。
            available = sorted_pool_units(&provisional, island_id);
            while !campaign_transport_package_covers(&remote_cargo, &transport_entities, catalog) {
                let Some(index) = available.iter().position(|transport| {
                    is_offshore_transport(transport.unit_type)
                        && remote_cargo.iter().any(|cargo| {
                            catalog.get(cargo).is_some_and(|cargo| {
                                cargo.island_id == transport.island_id
                                    && transport.loadable_unit_types.contains(&cargo.unit_type)
                            })
                        })
                }) else {
                    // 既存輸送が無くても作戦を消さない。不足量を後段のpurchase_shortfallへ上げる。
                    break;
                };
                let transport = available.remove(index);
                push_unique_entity(&mut transport_entities, transport.entity);
                reserved_entity_value = reserved_entity_value.saturating_add(transport.cost);
                remove_entity(&mut provisional, transport.entity);
            }
            if !campaign_transport_package_covers(&remote_cargo, &transport_entities, catalog) {
                // 生産可能な輸送手段が無ければ、安全に完全パッケージを作れない。
                let demand = remote_transport_shortfall(
                    &remote_cargo,
                    &transport_entities,
                    catalog,
                    &candidate.producible_transports,
                )?;
                remote_transport_demand = Some(demand);
            }
        }
    }

    let missing_lander = missing_assault_transports.contains(&UnitType::Lander);
    let missing_helicopter =
        missing_helicopter || missing_assault_transports.contains(&UnitType::TransportHelicopter);
    // 高価な既存unitのcreditで輸送・占領など必須カテゴリの購入費が消えないよう下限を戻す。
    let structural_floor = minimum_purchase_floor(
        candidate.assessment.decision,
        remaining_transport_slots,
        missing_lander,
        missing_helicopter,
        remaining_capture_units,
    );
    let base_purchase_budget = requirement
        .total_budget
        .saturating_sub(requirement_credit)
        .max(structural_floor);
    // 現地要員は輸送不要なため、実際のremote cargoから判明した輸送費だけを加算する。
    let remote_transport_budget = remote_transport_demand
        .map(|(_, _, cost)| cost)
        .unwrap_or(0);
    let purchase_budget = base_purchase_budget.saturating_add(remote_transport_budget);
    if provisional.available_funds < purchase_budget && !allow_future_budget_reservation {
        return None;
    }
    // 最上位Assaultだけは現在資金を全額予約し、未達分を次ターンへ持ち越す。
    // shortfallには必要総額を残し、allocated_budgetには実際に拘束した現金だけを記録する。
    let reserved_funds = provisional.available_funds.min(purchase_budget);
    provisional.available_funds = provisional.available_funds.saturating_sub(reserved_funds);

    if let Some((_, slots, _)) = remote_transport_demand {
        remaining_transport_slots = remaining_transport_slots.max(slots);
    }
    let preferred_transport = if candidate.assessment.decision == IslandCampaignDecision::Assault {
        missing_assault_transports.first().copied()
    } else if let Some((unit_type, _, _)) = remote_transport_demand {
        Some(unit_type)
    } else if remaining_transport_slots > 0 {
        requirement.preferred_transport
    } else {
        None
    };
    let purchase_shortfall = IslandCampaignRequirement {
        preferred_transport,
        transport_slots: remaining_transport_slots,
        capture_units: remaining_capture_units,
        // 地形分断を越える装甲枠だけは、敵数ではなく実際の突破未達から保持する。
        ground_combat_units: remaining_ground_combat_units,
        combat_units: remaining_combat_units,
        total_budget: purchase_budget,
    };
    let capture_package_ready = purchase_shortfall.capture_units == 0
        || candidate.assessment.decision == IslandCampaignDecision::Reinforce
            && !capture_entities.is_empty();
    let structural_package_ready = purchase_shortfall.transport_slots == 0
        && purchase_shortfall.ground_combat_units == 0
        && capture_package_ready;
    // 中立の兵站前提だけは構造パッケージ完成時点で先行できる。
    // 敵領Assaultは敵戦力を上回る離散的な損耗余裕まで揃わなければ出航させない。
    let operation_ready = structural_package_ready
        && ((candidate.logistics_prerequisite
            && candidate.assessment.decision != IslandCampaignDecision::Assault)
            || purchase_shortfall.combat_units == 0);
    transport_entities.sort_by_key(|entity| entity.to_bits());
    capture_entities.sort_by_key(|entity| entity.to_bits());
    combat_entities.sort_by_key(|entity| entity.to_bits());

    Some((
        IslandCampaignAssignment {
            island_id,
            decision: candidate.assessment.decision,
            target_position: candidate.target_position,
            capture_target_positions: candidate.capture_target_positions.clone(),
            priority_enemy_types: candidate.priority_enemy_types.clone(),
            requirement: requirement.clone(),
            purchase_shortfall,
            allocated_budget: reserved_entity_value.saturating_add(reserved_funds),
            transport_entities,
            capture_entities,
            combat_entities,
            operation_ready,
            continued_from_existing_squad,
        },
        provisional,
    ))
}

/// 橋・山で分断された同一陸塊の進軍軸を実際に前進できる装甲戦力かを判定する。
/// 砲兵など移動力の低い支援unitを、この突破枠の充足と誤認しない。
pub(super) fn is_route_armored_unit(unit_type: UnitType) -> bool {
    matches!(
        unit_type,
        UnitType::Tank | UnitType::MdTank | UnitType::TankZ
    )
}

/// 全島評価を保持したまま、行動可能な候補だけへ共有資源を予約する。
///
/// `IslandCampaignPortfolio.islands` は作戦候補一覧ではなく盤面の全島評価である。
/// そのため、目標座標を持たないSecured/Observed島をallocatorから外しても、
/// その評価自体は捨てない。
pub(crate) fn allocate_campaign_portfolio_with_assessments(
    candidates: Vec<IslandCampaignCandidate>,
    mut pool: CampaignResourcePool,
    mut assessments: Vec<IslandCampaignAssessment>,
) -> IslandCampaignPortfolio {
    let logistics_prerequisite_pending = candidates
        .iter()
        .any(|candidate| candidate.logistics_prerequisite);
    let has_expansion_candidate = candidates
        .iter()
        .any(|candidate| candidate.assessment.decision == IslandCampaignDecision::Expand);
    let catalog: HashMap<_, _> = pool
        .units
        .iter()
        .cloned()
        .map(|unit| (unit.entity, unit))
        .collect();
    // 【一時的な診断専用】allocator入口のpoolを記録する。
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        for unit in &pool.units {
            eprintln!(
                "POOL_AUDIT stage=allocator_entry entity={:?} type={:?} can_capture={} can_secure_local={} island={:?} assigned_island={:?} transporting={}",
                unit.entity,
                unit.unit_type,
                unit.can_capture,
                unit.can_secure_local_property,
                unit.island_id,
                unit.assigned_island,
                unit.is_transporting,
            );
        }
        for candidate in &candidates {
            eprintln!(
                "POOL_AUDIT stage=candidate island={:?} decision={:?} capture_units={} transport_slots={} ground_combat_units={} target={:?} existing_op={}",
                candidate.assessment.island_id,
                candidate.assessment.decision,
                candidate.requirement.capture_units,
                candidate.requirement.transport_slots,
                candidate.requirement.ground_combat_units,
                candidate.target_position,
                candidate.existing_operation.is_some(),
            );
        }
    }
    let mut contest_protections = protect_contest_assets(&candidates, &mut pool);
    reserve_secure_capture_units(
        &candidates
            .iter()
            .filter(|candidate| candidate.requirement.capture_units == 0)
            .cloned()
            .collect::<Vec<_>>(),
        &mut pool,
    );
    // 【一時的な診断専用】先行予約（Contest保護・Secure現地要員）の適用後を記録する。
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        let remaining: Vec<_> = pool
            .units
            .iter()
            .map(|unit| (unit.entity, unit.can_capture, unit.assigned_island))
            .collect();
        eprintln!(
            "POOL_AUDIT stage=after_prereservation contest_protected={:?} remaining={:?}",
            contest_protections, remaining
        );
    }
    // V4の兵站計画などが候補の評価を更新している場合は、同じ島の全島評価へ
    // 最新の判断を反映する。候補化されなかった島はここで上書きされない。
    for candidate in &candidates {
        if let Some(assessment) = assessments
            .iter_mut()
            .find(|assessment| assessment.island_id == candidate.assessment.island_id)
        {
            *assessment = candidate.assessment.clone();
        } else {
            assessments.push(candidate.assessment.clone());
        }
    }
    let mut offenses: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.assessment.decision,
                IslandCampaignDecision::Expand
                    | IslandCampaignDecision::Secure
                    | IslandCampaignDecision::Contest
                    | IslandCampaignDecision::Reinforce
                    | IslandCampaignDecision::Assault
            ) && (candidate.assessment.decision != IslandCampaignDecision::Secure
                || candidate.requirement.capture_units > 0
                || candidate.existing_operation.is_some())
        })
        .cloned()
        .collect();
    offenses.sort_by_key(offensive_priority_key);
    let mut active_offensives = Vec::new();
    let mut active_offensive_count = 0_usize;
    for candidate in offenses {
        if logistics_prerequisite_pending
            && !candidate.logistics_prerequisite
            && candidate.existing_operation.is_none()
            && candidate.assessment.decision == IslandCampaignDecision::Assault
        {
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "前進兵站拠点の確保まで新規強襲を待機する",
                None,
            );
            continue;
        }
        let consumes_offensive_slot =
            candidate.assessment.decision != IslandCampaignDecision::Secure;
        if consumes_offensive_slot && active_offensive_count == MAX_ACTIVE_OFFENSIVES {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "同時攻勢上限のため監視する",
                None,
            );
            continue;
        }
        // 最上位の新規Assaultは全額がまだ無くても作戦として保持し、現在資金を
        // 下位Expandへ流さず複数ターンで完全パッケージを調達する。
        let allow_future_budget_reservation = matches!(
            candidate.assessment.decision,
            IslandCampaignDecision::Secure
                | IslandCampaignDecision::Contest
                | IslandCampaignDecision::Reinforce
        ) || candidate
            .existing_operation
            .as_ref()
            .is_some_and(|operation| operation.is_forming)
            || active_offensives.is_empty()
                && (candidate.assessment.decision == IslandCampaignDecision::Assault
                    || candidate.logistics_prerequisite);
        if let Some((assignment, provisional)) =
            reserve_candidate(&candidate, &pool, &catalog, allow_future_budget_reservation)
        {
            pool = provisional;
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                contest_protections.remove(&candidate.assessment.island_id);
            }
            update_assessment_for_assignment(&mut assessments, &assignment);
            active_offensives.push(assignment);
            active_offensive_count =
                active_offensive_count.saturating_add(usize::from(consumes_offensive_slot));
        } else {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            let has_active_expansion = active_offensives
                .iter()
                .any(|assignment| assignment.decision == IslandCampaignDecision::Expand);
            let (decision, reason) = if candidate.assessment.decision
                == IslandCampaignDecision::Reinforce
                && (has_active_expansion || has_expansion_candidate)
            {
                (
                    IslandCampaignDecision::Withdraw,
                    "より投資回収効率のよい中立島作戦を優先する",
                )
            } else if candidate.assessment.decision == IslandCampaignDecision::Reinforce {
                (
                    IslandCampaignDecision::Contest,
                    "完全増援を予約できないため現地作戦を継続する",
                )
            } else {
                (
                    IslandCampaignDecision::Observe,
                    "完全編成を予約できないため監視する",
                )
            };
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                decision,
                reason,
                None,
            );
        }
    }

    let mut defense_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Defend)
        .collect();
    defense_candidates.sort_by_key(defense_priority_key);
    let mut defenses = Vec::new();
    for candidate in defense_candidates {
        loop {
            if let Some((assignment, provisional)) =
                reserve_candidate(&candidate, &pool, &catalog, false)
            {
                pool = provisional;
                update_assessment_for_assignment(&mut assessments, &assignment);
                defenses.push(assignment);
                break;
            }
            let Some(released) = active_offensives.pop() else {
                mark_unallocated(
                    &mut assessments,
                    candidate.assessment.island_id,
                    IslandCampaignDecision::Observe,
                    "完全な防衛編成を予約できないため監視する",
                    None,
                );
                break;
            };
            release_assignment(&released, &mut pool, &catalog);
            mark_unallocated(
                &mut assessments,
                released.island_id,
                IslandCampaignDecision::Observe,
                "Threatened島防衛のため攻勢を一時停止",
                Some(IslandCampaignPauseCause::DefensePreemption),
            );
        }
    }

    assessments.sort_by_key(|assessment| assessment.island_id.0);
    IslandCampaignPortfolio {
        islands: assessments,
        active_offensives,
        defenses,
    }
}
