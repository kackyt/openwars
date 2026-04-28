# ai-mcp-debugger Specification

## Purpose
AIエンジンの内部状態取得、盤面評価値の計算結果、合法手のスコアリングなどを外部（LLMエージェント等）から直接呼び出すための機能（MCPツール群）を定義する。
これにより、CLIの画面出力限界を突破し、チャットUI上で高度なデバッグと戦術ロジックの改善検証を行う。

## Requirements

### Requirement: 盤面状態のシリアライズ (State Serialization)
SHALL: システムの現在の盤面状態（地形、全ユニット、各プレイヤーの資金や所有拠点）をJSONフォーマットでダンプするツールを提供する。

#### Scenario: `get_board_state` ツールの実行
- **WHEN** LLMが `get_board_state` ツールを呼び出したとき
- **THEN** 現在のゲームルールと、盤面上に存在する全ユニットのリスト（ID、座標、HP、保持弾薬等）を含むJSONデータが返される

### Requirement: 静的盤面評価の可視化 (Static Evaluation)
SHALL: 現在の盤面全体の優位性を計算する `evaluate_board` ロジックを外部から叩き、内訳を詳細に取得するツールを提供する。

#### Scenario: `evaluate_board` ツールの実行
- **WHEN** LLMが `evaluate_board` ツールを呼び出したとき
- **THEN** 各プレイヤーの「戦力スコア（動的価値の合計）」「陣地・拠点スコア」等の詳細な内訳（Breakdown）がJSONデータとして返される

### Requirement: ユニットの行動候補と評価スコアの取得 (Action Scoring)
SHALL: 特定の未行動ユニットが「どこへ移動し、だれを攻撃するか」の全合法手パターンと、それに対するAIの評価スコアを取得するツールを提供する。

#### Scenario: `get_valid_actions` ツールの実行
- **WHEN** LLMが特定のユニットIDを指定して `get_valid_actions` ツールを呼び出したとき
- **THEN** そのユニットが適法に行える「移動先セル」＋「その場でのアクション（攻撃・待機・占領）」のすべての組み合わせと、それに対してAIが割り当てた優先度スコア（heuristic score）の配列が返される

### Requirement: 仮想ターンの実行とログ取得 (Dry-run AI Turn)
SHALL: 実際のゲーム（CLI）の表示を進めることなく、裏側でAIに1ターン分の思考ロジックを回させ、どのようなコマンドを発行しようとしたかのログを取得するツールを提供する。

#### Scenario: `simulate_ai_turn` ツールの実行
- **WHEN** LLMが `simulate_ai_turn` ツールを呼び出したとき
- **THEN** 現在のAIアルゴリズム（貪欲法、MCTS等）が決定した「そのターンの全ユニットの行動予定リスト」がシミュレーション結果として返される

### Requirement: アクションの直接実行 (Execute Action)
SHALL: 特定のユニットの「移動」「攻撃」、または拠点での「生産」などのコマンドを、LLMからMCP経由で直接エンジンに送信し、実際に盤面の状態を更新（適用）するツールを提供する。

#### Scenario: `execute_action` ツールの実行
- **WHEN** LLMがアクションの種類（Move, Attack, Produce等）と引数（対象ユニットIDや対象セルなど）を指定して `execute_action` ツールを呼び出したとき
- **THEN** エンジンは指定されたアクションが合法手であれば適用し、盤面状態（エンティティの座標やHP、資金など）を更新するとともに、実行結果の成否を返す

### Requirement: マップのロード (Load Map)
SHALL: 指定した名称のマップを読み込み、ゲームエンジンを初期状態にするツールを提供しなければならない。

#### Scenario: `load_map` ツールの実行
- **WHEN** LLMが `map_name` を指定して `load_map` ツールを呼び出したとき
- **THEN** 指定されたマップデータがロードされ、以降のツール実行対象となる `World` が生成される

### Requirement: ユニットの動的生成 (Spawn Unit)
SHALL: 検証のために、指定した座標・勢力のユニットを盤面上に直接生成するツールを提供しなければならない。

#### Scenario: `spawn_unit` ツールの実行
- **WHEN** LLMが座標 (x, y)、ユニット種別、プレイヤーIDを指定して `spawn_unit` ツールを呼び出したとき
- **THEN** 指定されたセルが空いていれば、新しいユニットが生成され、HPや燃料が最大値で初期化される

### Requirement: 移動可能範囲の取得 (Get Reachable Tiles)
SHALL: 特定ユニットが現在の燃料や移動力、地形コストを考慮して到達可能な全セルを計算して返すツールを提供しなければならない。

#### Scenario: `get_reachable_tiles` ツールの実行
- **WHEN** LLMがユニットIDを指定して `get_reachable_tiles` ツールを呼び出したとき
- **THEN** 燃料消費や敵ユニットによるZOC（侵入不可）を考慮した、移動可能な座標リストが返される

### Requirement: フェーズの進行 (Next Phase)
SHALL: 現在のプレイヤーのターンやフェーズ（移動・攻撃フェーズ、終了処理等）を強制的に進めるツールを提供しなければならない。

#### Scenario: `next_phase` ツールの実行
- **WHEN** LLMが `next_phase` ツールを呼び出したとき
- **THEN** ゲームの状態（MatchState）が次のフェーズまたは次のプレイヤーのターンに移行する
