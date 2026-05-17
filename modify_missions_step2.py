import sys

with open("engine/src/ai/missions.rs", "r") as f:
    content = f.read()

# Remove Map and Registry clones
# Before:
#     let map = world.resource::<Map>().clone();
#     let registry = world.resource::<MasterDataRegistry>().clone();
# After: Move this to AFTER the world.query loop!

content = content.replace("    let map = world.resource::<Map>().clone();\n    let registry = world.resource::<MasterDataRegistry>().clone();\n", "")

# Insert map and registry references right before calculate_reachable_tiles
insert_point = "    let reachable = calculate_reachable_tiles("
replacement = """    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();

    let reachable = calculate_reachable_tiles("""

content = content.replace(insert_point, replacement)

with open("engine/src/ai/missions.rs", "w") as f:
    f.write(content)
