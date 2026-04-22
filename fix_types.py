with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# Fix i32 vs isize
text = text.replace("let mut min_dist = i32::MAX;", "let mut min_dist = isize::MAX;")
text = text.replace("if min_dist != i32::MAX {", "if min_dist != isize::MAX {")

# combat_place_score is `isize`, but `place_score` is `i32`?
# let mut place_score = 0; -> type inferred as `i32` probably because of `place_score += 1000 - ...` (where 1000 is i32)?
text = text.replace("let mut place_score = 0;", "let mut place_score: isize = 0;")
text = text.replace("let mut best_place_score = -1;", "let mut best_place_score: isize = -1;")
text = text.replace("const MAX_PLACE_SCORE: i32 = 1000;", "const MAX_PLACE_SCORE: isize = 1000;")
text = text.replace("const DISTANCE_PENALTY: i32 = 10;", "const DISTANCE_PENALTY: isize = 10;")
text = text.replace("min_dist as i32 * DISTANCE_PENALTY", "min_dist * DISTANCE_PENALTY")

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
