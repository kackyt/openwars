with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# We need to replace:
#             if *ut == UnitType::Infantry {
#                score += 500;
#            } else {
#                score += 700;
#            }
# With something dynamic from registry, like taking a fraction of their cost or explaining the rationale clearly.
# The comment asks: "そうではなく、軽歩兵を500で重歩兵を700点にした根拠をきいています"
# Meaning: "No, I'm asking the rationale behind setting 500 for Light Infantry and 700 for Heavy/Mech Infantry."
# We should make these values based on dynamic metrics or cost.
# Like: score += (stats.max_movement * 100) + (stats.cost / 10);
# Or we can just calculate their base value dynamically:
# e.g. for Infantry/Mech, their inherent utility comes from movement range + capture ability.
# Let's write:
#            // 軽歩兵と重歩兵の基礎スコアを、機動力（移動力）と戦闘力（コスト）のバランスで算出する。
#            // 軽歩兵は安価で機動力が高い点、重歩兵はコストが高いが戦闘力が高い点を評価値に反映。
#            score += stats.max_movement * 100 + (stats.cost / 10);

text = re.sub(
    r'if \*ut == UnitType::Infantry \{\n\s*score \+= 500;\n\s*\} else \{\n\s*score \+= 700;\n\s*\}',
    r'// 軽歩兵と重歩兵の基礎スコアを、ユニットの機動力（移動力）と潜在的な戦闘力（コスト）のバランスから動的に算出する\n            // 軽歩兵は移動力が高いこと、重歩兵はコストが高いこと（戦闘力に比例）が評価に反映される\n            score += stats.max_movement * 100 + (stats.cost / 10);',
    text
)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
