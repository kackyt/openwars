---
name: openwars-interactive-advisor
description: プレイヤーがOpenWarsの戦術アドバイス、推奨行動の提案、またはAIによるユニットのインタラクティブな盤面操作を求めている場合に使用する。Pythonの戦略AIスクリプトを用いて現在の戦況を分析し、最適な行動をレコメンドしてプレイヤーと対話しながら操作を実行する。
---
# OpenWars Interactive Advisor

OpenWarsのMCPサーバー (`openwars`) を使用し、プレイヤーと対話しながら自軍のユニットを操作し、勝利を目指すための手順。
本手順では、盤面情報を分析するPythonスクリプトを実行し、最適な行動をプレイヤーに提案（レコメンド）し、承認を得た上でMCPを通じて行動を実行する。

## ワークフロー

### 1. 初期セットアップ（マップと軍の選択）
- プレイヤーにプレイするマップ（`map_1` や `map_2` など）と、先攻（Player 1）または後攻（Player 2）のどちらの軍で遊ぶかを選択してもらう。
- 選択されたマップをMCPツール `load_map` を使ってロードする。
- 後攻（Player 2）が選択された場合は、敵（Player 1）のターンを先行して進行させるため、一度MCPツール `simulate_ai_turn` を実行する。
- 自軍のターンが回ってきた状態で、手順2に進む。

### 2. 現在の盤面情報の取得と保存
- MCPツール `get_board_state` を実行し、現在の盤面状況（ユニット、拠点、プレイヤー情報など）を取得する。
- 取得したJSONデータを [board_state.json](./scratch/board_state.json) として保存する。

### 3. 分析スクリプトの実行とレコメンド結果の生成
- [recommend_action.py](./scripts/recommend_action.py) を実行する。
  `run_command` 等を用いて、以下のコマンドを実行する。
  ```powershell
  python .rulesync/skills/openwars-interactive-advisor/scripts/recommend_action.py
  ```
- 実行結果として、自動的に [recommendations.json](./scratch/recommendations.json) が生成される。これには戦況のサマリーと各ユニットの推奨アクションが記録される。

### 4. プレイヤーへのレコメンド提示
- 生成された [recommendations.json](./scratch/recommendations.json) を読み込む。
- プレイヤーに対し、「現在の戦況サマリー」と「推奨する具体的な行動（ユニットごとの移動や攻撃など）」をわかりやすい日本語で提示する。
- プレイヤーからの指示や意思確認（例: 「推奨通りに進めて」「このユニットは待機させて」など）を待つ。

### 5. アクションの実行（盤面操作）
- プレイヤーの承認または指示に基づき、MCPツール `execute_action` を実行してアクションを盤面に反映させる。
- レコメンドに含まれる `action_type`（"Move", "MoveAndAttack", "Capture"）やパラメータ（`target_x`, `target_y`, `target_id`）を、`execute_action` の適切な引数（`"move"`, `"attack"`, `"capture"`）に変換して順次実行する。

### 6. ターンの進行と敵AIの実行
- 自軍の操作が完了した後、MCPツール `next_phase` を実行してフェーズを進め、さらに `simulate_ai_turn` を実行して敵AIのターンを進行させる。
- 自軍のターンが再び回ってきたら、手順2に戻って処理を繰り返す。

## ゲームMCPサーバーの保護に関する規則

> [!CAUTION]
> ゲーム用のMCPサーバー（`mcp-server`）は、ゲームのライブ状態（`GameState` / bevy_ecsのWorld）をプロセスのメモリ上にのみ保持しています。
> したがって、プロセスを強制終了（Kill）したり再起動したりすると、プレイヤーがプレイしていたゲームの盤面データはメモリから完全に消滅します。
> 
> AIエージェントは以下のルールを厳重に遵守しなければなりません：
> 
> 1. **プロセスの自律的な強制終了・再起動の絶対禁止**:
>    - `Stop-Process`、`taskkill`、`kill` などのコマンドを使って `mcp-server` や `cargo` プロセスを自律的に強制終了することは、いかなる場合でも禁止します。
>    - `cargo run` などをバックグラウンドで勝手に実行してサーバーを再起動しようとする行為も絶対に行わないでください。
> 2. **エラー発生時のエスカレーション（司令官への報告義務）**:
>    - MCPツールの呼び出しがエラーになったり、応答が途絶えたりした（タイムアウト）場合は、AIの自己判断で復旧を試みず、ただちにエラー内容を司令官に報告し、次の指示を仰いでください。
> 3. **MCP設定の高速化（タイムアウトの根本防止）**:
>    - `mcp_config.json` における `openwars` の起動コマンドは、`cargo run` を経由するのではなく、事前にビルドされたリリースバイナリ（`target/release/mcp-server.exe` など）の直接起動を指定し、起動のオーバーヘッドを極小に保ってください。
