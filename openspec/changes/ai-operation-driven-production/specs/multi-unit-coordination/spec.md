## MODIFIED Requirements

### Requirement: Multi-Unit Coordination

MUST: AIは1つの目標に対して、適切な数のユニットを連携させて割り当てなければならない。この作戦・割当モデルは海を隔てた島嶼目標に限定せず、同一陸塊の陸上前線および防衛前線にも適用しなければならない。

#### Scenario: Coordinated Assignment

- **WHEN** AIプランナーが目標攻略のためのミッションを生成するとき
- **THEN** 対象の島・拠点の規模に応じて必要な占領要員数を計算し、輸送容量の範囲で占領要員と戦闘要員を同じ侵攻波へ割り当てる。輸送待ちユニットと輸送役は、双方が到達可能な合法な Pickup 位置へ移動する

#### Scenario: 陸続きの前線への適用

- **WHEN** 攻略目標が自軍と陸続きで、輸送ユニットを必要としないとき
- **THEN** 輸送枠を含まない作戦として占領要員と戦闘要員の割当を行い、島嶼目標と同一の割当モデルで管理する

#### Scenario: 防衛目標への適用

- **WHEN** 自軍拠点が敵の脅威下にあるとき
- **THEN** 当該拠点を目標とする作戦を構築し、脅威となる敵戦力に応じた戦闘要員を割り当てる

#### Scenario: 自力展開可能な戦力を輸送待ちにしない

- **WHEN** 作戦へ予約された航空戦力が現在地から目標へ自力で到達できるとき
- **THEN** 当該戦力を輸送cargoへ含めず、目標島をスコープとするAttackまたはDefense任務へ直接割り当てる

#### Scenario: 複数cargoを同一手番に合流させる

- **WHEN** Pickup中の先頭cargoが行動済みで、同じ輸送Squadに未行動の後続cargoが存在するとき
- **THEN** 先頭cargoだけを見てPickupを停止せず、後続の行動可能cargoを合法な合流地点へ前進させる

#### Scenario: 同一輸送役から複数cargoを同一手番に降車させる

- **WHEN** Drop中の輸送役に複数の行動可能cargoがあり、各cargoに合法な降車地点が存在するとき
- **THEN** 1体目のDrop後も輸送役を当該手番の処理対象に残し、残るcargoを同一手番に降車させる

#### Scenario: 空の輸送ヘリで軽歩兵から着陸地点を確保する

- **WHEN** TransportHelicopterが全cargoを降車し、目標島の着陸地点をInfantryまたはMechだけが妨害しているとき
- **THEN** 輸送Squadを終了または帰還させる前に、当該TransportHelicopterへ敵軽歩兵を対象とするAttack責務を与える

#### Scenario: 局地護衛を終えた輸送ヘリを再利用する

- **WHEN** 一時Attack責務を持つTransportHelicopterの目標島から敵軽歩兵がいなくなったとき
- **THEN** 同じ機体とSquad identityをTransport/Returnへ戻し、Attack完了による遊兵として残さない

#### Scenario: 複数占領兵で島内施設を分担する

- **WHEN** 1つの作戦に占領兵2体と、両者が到達可能な未所有施設2か所以上が割り当てられているとき
- **THEN** 各占領兵へ異なる施設座標のCapture責務を1件ずつ与え、同じ主目標へ重複集中させない

#### Scenario: 強襲cargoを輸送容量内に制限する

- **WHEN** 自力展開できない占領要員と戦闘要員をAssaultの初動上陸波へ予約するとき
- **THEN** 全cargoの必要スロットが割当済み輸送手段の実容量以下になる組み合わせだけを同じ侵攻波へ割り当てる

#### Scenario: 発進後の侵攻波を固定する

- **WHEN** AssaultがForming、Pickup、TransitまたはDropへ遷移した後に追加の戦闘要員が利用可能になったとき
- **THEN** 追加戦闘要員を進行中の侵攻波へ追加入隊させず、既存波のPickupと輸送を継続する
