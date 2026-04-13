# 仕様: マスターデータのロード (Master Data Loading)

## 概要
ゲームのパラメーター（地形特性、ユニットステータスなど）をCSVファイルから読み込み、エンジン内で利用可能にする機能を定義します。

## MODIFIED Requirements

### Requirement: 地形定義の拡充 (Expanded Terrain Definition)
`landscape.csv` の読み込みにおいて、各地形の `defense_bonus`（0〜50の整数）および各種移動タイプ別の `move_cost` を正しくパースし、メモリ上の構造体に反映しなければならない (MUST)。

#### Scenario: 森林地形のデータ読み込み
- **GIVEN** `landscape.csv` に山(Mountain)の防御ボーナス 40, 歩兵コスト 2 が定義されている
- **WHEN** ゲームが起動し、マスターデータがロードされたとき
- **THEN** `MasterDataRegistry` が「山」に対して防御ボーナス 40 保持し、システムから参照可能である。

### Requirement: 初期化時の同期読み込み (Synchronous Loading on Init)
ゲーム起動時、またはマップ初期化時に `landscape.csv`, `unit_types.csv` などの全マスターデータを読み込み、全システムが利用可能な状態（Resourceの挿入）にしなければならない (MUST)。

#### Scenario: ログイン後のマップ展開
- **WHEN** 対戦（マッチ）が開始されたとき
- **THEN** 全てのユニットや地形の定義が読み込まれ、型安全な `MasterDataRegistry` を介してシステムが動作を開始できる。
