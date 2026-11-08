//! 明示的に有効化した場合だけ、同じ生産入力で砲台輸送の評価差を記録する診断。
//! 実際に選択・発注する計画は変えない。輸送を消す変種は反実仮想であり、採用候補ではない。
use crate::ai::v4::rolling_plan::{self, ForcePackagePlan, RollingPlanInput};
use crate::components::{GridPosition, PlayerId};
use serde_json::{Value, json};
use std::io::Write;

fn summary(plan: Option<ForcePackagePlan>) -> Value {
    let Some(plan) = plan else {
        return Value::Null;
    };
    json!({
        "purchases": plan.purchases.iter().map(|p| json!({
            "unit_type": p.unit_type.as_str(), "cost":p.cost,"build_turn":p.build_turn,
            "facility":[p.facility.x,p.facility.y]
        })).collect::<Vec<_>>(),
        "feasible":plan.feasible,"first_attack":plan.first_attack_turn,
        "elimination":plan.elimination_turn,"cost":plan.production_cost,
        "loss":plan.expected_loss,"remaining_enemy_hp":plan.target_forecasts.iter().map(|t|t.remaining_hp).sum::<u32>(),
        "direct_shortfall":plan.direct_front_shortfall,
        "existing_indirect_damage":plan.existing_indirect_damage_by_target,
        "new_indirect_damage":plan.produced_indirect_damage_by_target,
    })
}

/// 出力先を指定した診断だけ実行する。生産入力は複製し、実発注には触れない。
pub(super) fn record(input: &RollingPlanInput, turn: u32, player: PlayerId, anchor: GridPosition) {
    let Ok(path) = std::env::var("OPENWARS_PRODUCTION_ABLATION_OUT") else {
        return;
    };
    if let Ok(selected) = std::env::var("OPENWARS_PRODUCTION_ABLATION_TURN")
        && selected.parse::<u32>().ok() != Some(turn)
    {
        return;
    }
    let mut without_transport = input.clone();
    without_transport.artillery_carrier = None;
    // ペア価格を維持した比較で移動・補給の仮定だけを分離する。
    let same_price = summary(rolling_plan::plan_force_package(&without_transport));
    // 徒歩砲の価格まで戻す比較も別に記録する（mainそのものの再現ではない）。
    for option in &mut without_transport.production_options {
        if crate::ai::v4::artillery_ferry::is_ferried_gun(&option.stats) {
            option.purchase.cost = option.stats.cost;
        }
    }
    let row = json!({
        "turn":turn,"player":player.0,"anchor":[anchor.x,anchor.y],
        "funds":input.current_funds,"income":input.income_per_turn,
        "carrier_model":input.artillery_carrier.as_ref().map(|c|c.unit_type.as_str()),
        "spare_carriers":input.spare_artillery_carriers,
        "carrier_now":input.carrier_facilities_now.len(),
        "carrier_later":input.carrier_facilities_later.len(),
        "option_turns":input.production_options.iter().map(|o|o.purchase.build_turn).max(),
        "option_facilities_t0":input.production_options.iter().filter(|o|o.purchase.build_turn==0).map(|o|(o.purchase.facility.x,o.purchase.facility.y)).collect::<std::collections::BTreeSet<_>>().len(),
        "existing":input.existing_units.iter().map(|u|json!({"type":u.stats.unit_type.as_str(),"hp":u.hp,"position":[u.position.x,u.position.y],"available":u.available_turn})).collect::<Vec<_>>(),
        "protected_count":input.protected_units.len(),
        "enemies":input.enemies.len(),"options":input.production_options.len(),
        "transport":summary(rolling_plan::plan_force_package(input)),
        "no_transport_same_price":same_price,
        "no_transport_gun_price":summary(rolling_plan::plan_force_package(&without_transport)),
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("生産比較診断の出力先を開けない");
    writeln!(file, "{row}").expect("生産比較診断を書き出せない");
}
