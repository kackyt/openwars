# property-capture Specification

## Purpose
TBD - created by archiving change implement-capture. Update Purpose after archive.
## Requirements
### Requirement: 拠点の耐久値と中立状態
拠点は地形種別ごとに定められた最大占領耐久値を持ち、どのプレイヤーにも属さない「中立」状態をとることができなければならない。The properties MUST have distinct max durability values and support a neutral state.
- 首都 (Capital): 最大耐久値 400
- 空港 (Airport), 港 (Port): 最大耐久値 300
- 都市 (City), 工場 (Factory): 最大耐久値 200

#### Scenario: 拠点の初期状態
- **GIVEN** マップ上に拠点が配置されている
- **WHEN** ゲームが開始される、または中立の拠点が生成されるとき
- **THEN** その拠点はプレイヤーに所有されていない中立状態となる
- **AND** 占領耐久値は拠点種別ごとの最大値（首都400、空港・港300、都市・工場200）に設定される

### Requirement: 占領アクションの実行
「歩兵」または「戦闘工兵」は、中立または敵国の拠点の上にいるとき、アクションとして「占領」を行うことができなければならない。Infantry and combat engineers MUST be able to execute a capture action on neutral or enemy properties.

#### Scenario: 占領の進行
- **GIVEN** アクティブプレイヤーが未行動の「歩兵または戦闘工兵」を選択している
- **AND** ユニットが中立または敵国の拠点マスの上にいる
- **WHEN** プレイヤーが「占領」アクションを実行したとき
- **THEN** その拠点の占領耐久値から、現在のアクティブなユニットの「HP × 10（通常10〜100）」を減算する
- **AND** そのユニットは行動完了状態になる

#### Scenario: 占領の完了と所有権の確保
- **GIVEN** 拠点の残りの占領耐久値が、現在その上にいる占領アクション実行可能ユニットの「HP × 10」以下である
- **WHEN** プレイヤーがそのユニットで「占領」アクションを実行したとき
- **THEN** その拠点の所有権がアクティブプレイヤーのものに変更される
- **AND** その拠点の占領耐久値が、拠点種別ごとの最大値（200, 300, 400など）にリセットされる
- **AND** 対象が「敵の首都」であった場合、既存の勝利条件（game-state仕様）が満たされる

### Requirement: 修復アクションの実行
「歩兵」または「戦闘工兵」は、自国の拠点の上にいるとき、アクションとして「修復」を行うことができなければならない。These units MUST be able to execute a repair action on friendly properties.

#### Scenario: 拠点の修復
- **GIVEN** アクティブプレイヤーが未行動の「歩兵または戦闘工兵」を選択している
- **AND** ユニットが耐久値の減っている自国の拠点マスの上にいる
- **WHEN** プレイヤーが「修復」アクションを実行したとき
- **THEN** その拠点の占領耐久値を、現在のアクティブなユニットの「HP × 10」分回復する
- **AND** 回復後の耐久値は、各拠点の最大耐久値を超えない
- **AND** そのユニットは行動完了状態になる

### Requirement: 占領状態の継続（自然回復しない）
一度減らされた占領耐久値は、自然回復せず維持されなければならない。The capture points MUST persist and not automatically reset even if the capturing unit leaves or is destroyed.

#### Scenario: ユニットがマスから離れた場合の耐久値維持
- **GIVEN** 拠点の占領耐久値が最大値未満である
- **WHEN** そのマス上で占領を行っていたユニットが他のマスへ移動した、または撃破（消滅）したとき
- **THEN** その拠点の占領耐久値は回復せず、減らされた状態のまま維持される
- **AND** 他の歩兵が到達した場合、その減った状態から引き続き占領を行うことができる

