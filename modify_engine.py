import sys

with open("engine/src/ai/engine.rs", "r") as f:
    content = f.read()

old_mission_execute = """    // ミッションを持つユニットがいたら優先して実行
    let mut mission_to_execute = None;
    let mut transport_entities = vec![];
    if let Some(manager) = world.get_resource::<crate::ai::missions::TransportMissionManager>() {
        for mission in &manager.missions {
            if !skip_entities.contains(&mission.transport_entity) {
                transport_entities.push((mission.transport_entity, mission.clone()));
            }
        }
    }

    let mut query = world.query::<&Faction>();
    for (entity, mission) in transport_entities {
        if let Ok(faction) = query.get(world, entity)
            && faction.0 == active_player {
                mission_to_execute = Some(mission);
                break;
            }
    }"""

new_mission_execute = """    // ミッションを持つユニットがいたら優先して実行
    let mission_to_execute = world
        .get_resource::<crate::ai::missions::TransportMissionManager>()
        .and_then(|manager| {
            manager.missions.iter().find(|m| {
                !skip_entities.contains(&m.transport_entity)
                    && world
                        .get::<Faction>(m.transport_entity)
                        .map_or(false, |f| f.0 == active_player)
            }).copied() // Since TransportMission derives Copy now
        });"""

content = content.replace(old_mission_execute, new_mission_execute)

with open("engine/src/ai/engine.rs", "w") as f:
    f.write(content)
