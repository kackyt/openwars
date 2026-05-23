## MODIFIED Requirements

### Requirement: ゲームフェーズの動的判定
MUST: AIは、マップの状態から現在のゲームフェーズを判定し、戦略を決定しなければならない。フェーズ判定における「敵との距離」は TurnDistance を用いて計算しなければならない（マス単位のマンハッタン距離は使用しない）。

#### Scenario: 序盤の拡張フェーズ
- **WHEN** マップ上に中立の拠点が 30% 以上存在する場合
- **THEN** 戦略を「拡張（Expansion）」に設定し、歩兵ユニットの優先度を最大化する

#### Scenario: 中盤の対峙フェーズ
- **WHEN** 中立拠点が少なくなり、TurnDistance ベースで敵ユニットが 2 ターン以内に到達可能なユニットが複数存在する場合
- **THEN** 戦略を「均衡（Contested）」に設定し、前線維持のための戦闘ユニットと占領用ユニットをバランスよく配置する

#### Scenario: 終盤の攻勢フェーズ
- **WHEN** 敵の首都周辺（TurnDistance ≤ 3）に自軍ユニットが進出している場合
- **THEN** 戦略を「攻勢（Assault）」に設定し、高コストな強力ユニットの生産優先度を上げる

### Requirement: 敵の脅威判定（TurnDistance ベース）
SHALL: `enemy_threatens_property` は、敵ユニットから対象拠点への `TurnDistance` が `THREAT_THRESHOLD_TURNS`（デフォルト 3）以下の場合に脅威ありと判定しなければならない。移動タイプ（Air/Ship/Ground）に関わらず同一の TurnDistance 計算を用い、マス距離による移動タイプ別の閾値分岐は廃止する。

#### Scenario: 航空ユニットが拠点に 2 ターンで到達できる場合
- **WHEN** 敵の戦闘ヘリが自軍工場へ TurnDistance = 2 の位置にいる
- **THEN** 脅威ありと判定し、DefenseMission の形成トリガーとなる

#### Scenario: 地上ユニットが海を挟んで存在する場合
- **WHEN** 敵地上ユニットが海を隔てた場所にいて TurnDistance = ∞
- **THEN** 脅威なしと判定する（従来のマス距離では誤って脅威ありと判定していた）
