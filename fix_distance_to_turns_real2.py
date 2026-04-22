with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

text = text.replace("crate::resources::Terrain::Plains", '"平地"')
text = text.replace("(dist * avg_move_cost).div_ceil(max_movement)", "((dist * avg_move_cost) + max_movement - 1) / max_movement")
text = text.replace("(target_dist * avg_move_cost).div_ceil(max_movement)", "((target_dist * avg_move_cost) + max_movement - 1) / max_movement")

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
