import sys

with open("engine/src/ai/planner.rs", "r") as f:
    content = f.read()

# Replace has_mission
old_has_mission = """    // すでに進行中のミッションがある場合はスキップ (簡略化)
    let has_mission = {
        let mut found = false;
        let mut missions_entities = vec![];
        if let Some(manager) = world.get_resource::<TransportMissionManager>() {
            for m in &manager.missions {
                missions_entities.push(m.transport_entity);
            }
        }
        let mut query = world.query::<&Faction>();
        for entity in missions_entities {
            if let Ok(faction) = query.get(world, entity)
                && faction.0 == player_id {
                    found = true;
                    break;
                }
        }
        found
    };"""

new_has_mission = """    // すでに進行中のミッションがある場合はスキップ (簡略化)
    let has_mission = world
        .get_resource::<TransportMissionManager>()
        .map_or(false, |manager| {
            manager.missions.iter().any(|m| {
                world
                    .get::<Faction>(m.transport_entity)
                    .map_or(false, |f| f.0 == player_id)
            })
        });"""

content = content.replace(old_has_mission, new_has_mission)

# Replace free infantry search
old_infantry_search = """    // 2. フリーな歩兵を探す（他のCargoに入っていないかチェック）
    let mut free_infantry = None;
    {
        // まず、すでに搭載されているエンティティのリストを作る
        let mut loaded_entities = std::collections::HashSet::new();
        let mut query_cargo = world.query::<&CargoCapacity>();
        for cargo in query_cargo.iter(world) {
            for &e in &cargo.loaded {
                loaded_entities.insert(e);
            }
        }

        let mut query_inf = world.query::<(Entity, &Faction, &UnitStats)>();
        for (entity, faction, stats) in query_inf.iter(world) {
            if faction.0 == player_id
                && stats.unit_type == UnitType::Infantry
                && !loaded_entities.contains(&entity)
            {
                free_infantry = Some(entity);
                break;
            }
        }
    }"""

new_infantry_search = """    // 2. フリーな歩兵を探す（他のCargoに入っていないかチェック）
    let mut free_infantry = None;
    {
        let mut query_inf = world.query::<(Entity, &Faction, &UnitStats, Option<&crate::components::Transporting>)>();
        for (entity, faction, stats, transporting_opt) in query_inf.iter(world) {
            if faction.0 == player_id
                && stats.unit_type == UnitType::Infantry
                && transporting_opt.is_none()
            {
                free_infantry = Some(entity);
                break;
            }
        }
    }"""

content = content.replace(old_infantry_search, new_infantry_search)

with open("engine/src/ai/planner.rs", "w") as f:
    f.write(content)
