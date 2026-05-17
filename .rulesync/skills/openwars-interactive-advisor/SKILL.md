---
name: openwars-interactive-advisor
description: プレイヤーがOpenWarsの戦術アドバイス、推奨行動の提案、またはAIによるユニットのインタラクティブな盤面操作を求めている場合に使用する。Pythonの戦略AIスクリプトを用いて現在の戦況を分析し、最適な行動をレコメンドしてプレイヤーと対話しながら操作を実行する。
---
# OpenWars Interactive Advisor

OpenWarsのMCPサーバー (`openwars`) を使用し、プレイヤーと対話しながら自軍のユニットを操作し、勝利を目指すための手順。
本手順では、盤面情報を分析するPythonスクリプトを実行し、最適な行動をプレイヤーに提案（レコメンド）し、承認を得た上でMCPを通じて行動を実行する。

## ワークフロー

### 1. 現在の盤面情報の取得と保存
- MCPツール `get_board_state` を実行し、現在の盤面状況（ユニット、拠点、プレイヤー情報など）を取得する。
- 取得したJSONデータを [board_state.json](./scratch/board_state.json) として保存する。

### 2. 分析スクリプトの実行とレコメンド結果の生成
- [recommend_action.py](./scripts/recommend_action.py) を実行する。
  `run_command` 等を用いて、以下のコマンドを実行する。
  ```powershell
  python .rulesync/skills/openwars-interactive-advisor/scripts/recommend_action.py
  ```
- 実行結果として、自動的に [recommendations.json](./scratch/recommendations.json) が生成される。これには戦況のサマリーと各ユニットの推奨アクションが記録される。

### 3. プレイヤーへのレコメンド提示
- 生成された [recommendations.json](./scratch/recommendations.json) を読み込む。
- プレイヤーに対し、「現在の戦況サマリー」と「推奨する具体的な行動（ユニットごとの移動や攻撃など）」をわかりやすい日本語で提示する。
- プレイヤーからの指示や意思確認（例: 「推奨通りに進めて」「このユニットは待機させて」など）を待つ。

### 4. アクションの実行（盤面操作）
- プレイヤーの承認または指示に基づき、MCPツール `execute_action` を実行してアクションを盤面に反映させる。
- レコメンドに含まれる `action_type`（"Move", "MoveAndAttack", "Capture"）やパラメータ（`target_x`, `target_y`, `target_id`）を、`execute_action` の適切な引数（`"move"`, `"attack"`, `"capture"`）に変換して順次実行する。

### 5. ターンの進行と敵AIの実行
- 自軍の操作が完了した後、MCPツール `next_phase` を実行してフェーズを進め、さらに `simulate_ai_turn` を実行して敵AIのターンを進行させる。
- 自軍のターンが再び回ってきたら、手順1に戻って処理を繰り返す。
