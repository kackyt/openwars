import sys

with open("engine/src/ai/missions.rs", "r") as f:
    content = f.read()

# Replace English comments with Japanese

# Phase enums
content = content.replace("#[derive(Debug, Clone, Copy, PartialEq, Eq)]", "/// 輸送ミッションの各フェーズ\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
content = content.replace("    Pickup,", "    Pickup,  // 歩兵の回収に向かうフェーズ")
content = content.replace("    Transit,", "    Transit, // 目標の島へ海上を移動するフェーズ")
content = content.replace("    Drop,", "    Drop,    // 目標の島に歩兵を降ろすフェーズ")
content = content.replace("    Return,", "    Return,  // 任務完了後、拠点に帰還するフェーズ")

# Mission struct
content = content.replace("#[derive(Debug, Clone)]", "/// 輸送ミッションの情報\n#[derive(Debug, Clone, Copy)]")

# execute_mission_step
content = content.replace("    // Basic checks", "    // 輸送機の基本情報を取得")
content = content.replace("    // Occupant info for pathfinding", "    // 経路探索のために他ユニットの占有情報を取得")
content = content.replace("            // For now, let's just make the transport move towards the cargo and wait.", "            // 対象の歩兵の現在位置に最も近いタイルへ移動して待機する")
content = content.replace("                // Move transport towards target island if we can't drop", "                // 降ろせる場所がない場合は、待機して機を伺う")

with open("engine/src/ai/missions.rs", "w") as f:
    f.write(content)
