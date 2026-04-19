# ai-mcp-debugger Specification

## Purpose
AIエンジンの内部状態取得、盤面評価値の計算結果、合法手のスコアリングなどを外部（LLMエージェント等）から直接呼び出すための機能（MCPツール群）を定義する。
これにより、CLIの画面出力限界を突破し、チャットUI上で高度なデバッグと戦術ロジックの改善検証を行う。

## Requirements

### Requirement: 盤面状態のシリアライズ (State Serialization)
システムの現在の盤面状態（地形、全ユニット、各プレイヤーの資金や所有拠点）をJSONフォーマットでダンプするツールを提供する (SHALL)。

#### Scenario: `get_board_state` ツールの実行
- **WHEN** LLMが `get_board_state` ツールを呼び出したとき
- **THEN** 現在のゲームルールと、盤面上に存在する全ユニットのリスト（ID、座標、HP、保持弾薬等）を含むJSONデータが返される

### Requirement: 静的盤面評価の可視化 (Static Evaluation)
現在の盤面全体の優位性を計算する `evaluate_board` ロジックを外部から叩き、内訳を詳細に取得するツールを提供する (SHALL)。

#### Scenario: `evaluate_board` ツールの実行
- **WHEN** LLMが `evaluate_board` ツールを呼び出したとき
- **THEN** 各プレイヤーの「戦力スコア（動的価値の合計）」「陣地・拠点スコア」等の詳細な内訳（Breakdown）がJSONデータとして返される

### Requirement: ユニットの行動候補と評価スコアの取得 (Action Scoring)
特定の未行動ユニットが「どこへ移動し、だれを攻撃するか」の全合法手パターンと、それに対するAIの評価スコアを取得するツールを提供する (SHALL)。

#### Scenario: `get_valid_actions` ツールの実行
- **WHEN** LLMが特定のユニットIDを指定して `get_valid_actions` ツールを呼び出したとき
- **THEN** そのユニットが適法に行える「移動先セル」＋「その場でのアクション（攻撃・待機・占領）」のすべての組み合わせと、それに対してAIが割り当てた優先度スコア（heuristic score）の配列が返される

### Requirement: 仮想ターンの実行とログ取得 (Dry-run AI Turn)
実際のゲーム（CLI）の表示を進めることなく、裏側でAIに1ターン分の思考ロジックを回させ、どのようなコマンドを発行しようとしたかのログを取得するツールを提供する (SHALL/OPTIONAL)。

#### Scenario: `simulate_ai_turn` ツールの実行
- **WHEN** LLMが `simulate_ai_turn` ツールを呼び出したとき
- **THEN** 現在のAIアルゴリズム（貪欲法、MCTS等）が決定した「そのターンの全ユニットの行動予定リスト」がシミュレーション結果として返される

### Requirement: アクションの直接実行 (Execute Action)
特定のユニットの「移動」「攻撃」、または拠点での「生産」などのコマンドを、LLMからMCP経由で直接エンジンに送信し、実際に盤面の状態を更新（適用）するツールを提供する (SHALL)。

#### Scenario: `execute_action` ツールの実行
- **WHEN** LLMがアクションの種類（Move, Attack, Produce等）と引数（対象ユニットIDや対象セルなど）を指定して `execute_action` ツールを呼び出したとき
- **THEN** エンジンは指定されたアクションが合法手であれば適用し、盤面状態（エンティティの座標やHP、資金など）を更新するとともに、実行結果の成否を返す
