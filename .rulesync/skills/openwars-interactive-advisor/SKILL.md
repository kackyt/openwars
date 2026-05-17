---
name: openwars-interactive-advisor
description: プレイヤーと対話しながら openwars MCP を操作し、相手軍からの勝利を目指すスキルです。Pythonの戦略AIスクリプトを実行して、効果的な行動をプレイヤーにレコメンドし、指示に従って操作します。
---
# OpenWars Interactive Advisor

OpenWarsのMCPサーバー (`openwars`) を使用して、プレイヤーと対話しなら自軍のユニットを操作し、勝利を目指すためのスキルです。
このスキルは、盤面情報を分析するPythonスクリプトを実行し、最適な行動をプレイヤーに提案（レコメンド）して、承認を得た上でMCPを通じて行動を実行します。

## ワークフロー

このスキルを実行する際は、以下の手順に従ってください。

### 1. 現在の盤面情報の取得と保存
- 実行するMCPツール: `get_board_state`
- 目的: 現在の盤面状況（ユニット、拠点、プレイヤー情報など）を取得します。
- 保存: 取得したJSONデータを `.rulesync/skills/openwars-interactive-advisor/scratch/board_state.json` というファイル名で保存します。

### 2. 分析スクリプトの実行とレコメンド結果の生成
- 実行するコマンド: `run_command` (または `run_in_bash_session`) を使用して、`python3 .rulesync/skills/openwars-interactive-advisor/recommend_action.py` を実行します。
- 目的: 保存した `.rulesync/skills/openwars-interactive-advisor/scratch/board_state.json` を分析し、各ユニットの推奨アクションを決定します。
- 出力: スクリプトは `.rulesync/skills/openwars-interactive-advisor/scratch/recommendations.json` に分析結果（戦況サマリーと推奨アクション）を出力します。

### 3. プレイヤーへのレコメンド
- アクション: 生成された `.rulesync/skills/openwars-interactive-advisor/scratch/recommendations.json` を読み込みます。
- 目的: プレイヤーに対して、「現在の戦況サマリー」と「推奨する具体的な行動（ユニットごとの移動や攻撃など）」をわかりやすい日本語で提示します。
- 注意点: 専門用語だけでなく、直感的に状況がわかるように説明し、プレイヤーからの指示や意思確認（例: 「推奨通りに進めて」「このユニットは待機させて」など）を待ちます。

### 4. アクションの実行（盤面操作）
- 実行するMCPツール: `execute_action`
- 目的: プレイヤーからの具体的な指示や承認に基づき、推奨アクションを盤面に反映させます。
- 注意点: レコメンドにある `action_type` (例: "Move", "MoveAndAttack", "Capture") やパラメータ (`target_x`, `target_y`, `target_id`) を適切な `execute_action` への引数（`"move"`, `"attack"`, `"capture"`）に変換して順次実行します。

### 5. ターンの進行とAIの実行
- 実行するMCPツール: `next_phase` および `simulate_ai_turn`
- 目的: 自軍の操作が終了したら、プレイヤーのターンを終了させてフェーズを進め（`next_phase`）、敵AIのターンを実行（`simulate_ai_turn`）します。
- サイクル: 再び自軍のターンに戻ってくるまでターンを進め、再び 1. の盤面情報の取得から繰り返します。
