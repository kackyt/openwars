## MODIFIED Requirements

### Requirement: 戦術的なアクション決定
MUST: AIは各ターン開始時に `plan_full_turn()` を呼び出し、`SquadManager` による部隊再編成と `BeamSearch` による全 Squad 目標割り当て最適化を実行しなければならない。Squad に属するユニットは Squad のミッションフェーズに従って行動する。Squad に属さない SoloFallback ユニットのみ、従来の `decide_ai_action` 貪欲法で個別に最善手を選択する。

#### Scenario: Squad に属するユニットが Squad の指示に従って行動する場合
- **WHEN** AttackMission の Squad が Converge フェーズにある
- **THEN** Squad のメンバーはターゲットクラスターに向かって移動し、個別の最高スコアアクションとは独立してミッション行動を優先する

#### Scenario: SoloFallback ユニットが貪欲法で行動する場合
- **WHEN** HP < 60 のユニットが Squad に属していない
- **THEN** `decide_ai_action` で個別に最善手を選択するが、評価スコアに最寄り Squad への接近ボーナスが加算される

### Requirement: スコアリングに基づく位置取り
SHALL: ユニットの移動先を決定する際、AIは目的地の地形防御ボーナスおよびターゲットへの「到達ターン数（TurnDistance）」を考慮して評価値を算出しなければならない。マス単位のマンハッタン距離による評価は使用しない。

#### Scenario: 地形効果の活用
- **WHEN** 平地と山が隣接しており、どちらからも攻撃可能な場合
- **THEN** 防御ボーナスの高い山を移動先として選択する

#### Scenario: ターン数距離を用いた到達評価
- **WHEN** 目標拠点が 5 マス先にあるが山岳地帯を通るため実際には 3 ターンかかる
- **THEN** 評価スコアは「3 ターン先に到達できる」として計算し、マス数の 5 ではなく TurnDistance の 3 を用いる
