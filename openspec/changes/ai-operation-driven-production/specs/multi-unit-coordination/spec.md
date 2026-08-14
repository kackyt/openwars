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

#### Scenario: 輸送役へ到達したcargoを直ちに積載する

- **WHEN** Pickup中の未行動cargoが現在の行動範囲で輸送役の座標へ到達できるとき
- **THEN** 輸送役自身の移動より先にcargoを選び、cargoの移動と積載を同じLoadコマンドで確定する。同一マス占有を残すWaitが発行された場合、エンジンは待機を拒否し、当該cargoの直前の移動を巻き戻す

#### Scenario: 兵站便を遠い2体目のcargo待ちで停止させない

- **WHEN** Expand、Secure、ContestまたはReinforceのPickup便が1体以上を搭載済みで、残る割当cargoが現在の手番に同じ輸送役へ到達できないとき
- **THEN** 搭載済みcargoを運ぶ便をTransitへ進め、未搭載cargoは同じ目標島の後続Formingへ戻す。Assaultではこの部分発進を行わず、輸送役ごとの完全manifestを待つ

#### Scenario: 島全体の要求完成前に実行可能な兵站便を発進させる

- **WHEN** Expand、Secure、ContestまたはReinforceの島全体要求は未完成だが、Formingに実輸送役とその容量内の互換cargoが存在するとき
- **THEN** 容量内のcargoを輸送役ごとのPickupへ昇格し、容量外のcargoと未使用輸送役は同じ島の後続Formingに保持する。Assaultでは全要求が完成するまでこの昇格を行わない

#### Scenario: 購入待ちplaceholderへ生産済みEntityを接続する

- **WHEN** 空のForming placeholderが存在する島作戦へ、後の手番で輸送役またはremote cargoが割り当てられたとき
- **THEN** 新しいEntityを同じplaceholderへ追加入隊させ、代表輸送役だけでなく輸送Squadの全membersとcargoを汎用行動および別作戦の重複割当から除外する

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

#### Scenario: 大陸・島を問わず複数の前線施設を保持する

- **WHEN** 首都攻略へ至る経路上に、自力到達または輸送で取得可能な中立・敵所有施設が複数存在するとき
- **THEN** 代表座標1件や固定件数だけを保持せず、全施設を作戦目的として監視し、既存占領Entityと生産・輸送可能な後続Entityを異なる目的へ並行割当する

#### Scenario: 未所有施設が残る島を継続監視する

- **WHEN** 前ターンまでに上陸部隊を送った島に中立または敵所有の施設が残っているとき
- **THEN** 初回便の有無にかかわらず毎ターン残存施設数を再計測し、未所有施設1か所につき占領兵1体の要求を維持または再生成する。上陸・争奪中は最低2体とし、安全な自島内の仕上げは残存施設数ちょうどとする

#### Scenario: 安全な自島の仕上げを攻勢上限の外で行う

- **WHEN** 敵戦力がいない自軍支配島に未所有施設が残り、別の島への攻勢候補も存在するとき
- **THEN** 当該島のSecure占領任務を生成しつつ、通常の島攻勢を最大3件まで並行できるようにする

#### Scenario: 強襲cargoを輸送容量内に制限する

- **WHEN** 自力展開できない占領要員と戦闘要員をAssaultの初動上陸波へ予約するとき
- **THEN** 全cargoの必要スロットが割当済み輸送手段の実容量以下になる組み合わせだけを同じ侵攻波へ割り当てる

#### Scenario: 実輸送開始後の侵攻波を固定する

- **WHEN** AssaultがPickup、TransitまたはDropへ遷移した後に追加の戦闘要員が利用可能になったとき
- **THEN** 追加戦闘要員を進行中の侵攻波へ追加入隊させず、既存波のPickupと輸送を継続する。Forming中は要求戦力を満たす後着要員を再編できる

#### Scenario: 敵領強襲は優越戦力を揃えてから発進する

- **WHEN** 敵戦力が残る島へのAssaultで輸送手段と占領要員は揃ったが、局地敵戦力を最小生産戦闘unit 1体分だけ上回る戦闘予算が未充足のとき
- **THEN** 作戦をFormingに保ち、戦闘不足を満たす要員を同じ侵攻波へ追加できるようにし、Pickupへ遷移させない

#### Scenario: Pickupで生産施設を搭載地点にしない

- **WHEN** Pickup中の輸送役が首都の生産圏内にある自軍生産施設上にいて、到達可能な非生産合流点が存在するとき
- **THEN** 即時Loadより先に輸送役を非生産合流点へ移動し、同じ手番の後続行動でcargoを移動Loadして次の生産フェーズまでに施設を空ける

#### Scenario: 航空任務は帰投燃料を残す

- **WHEN** 航空unitが攻撃対象またはSquad目標へ向かう候補タイルを評価するとき
- **THEN** 候補までの移動後燃料で、最寄りの自軍空港までの距離と帰投中の日次燃料消費を支払えないタイルを行動候補から除外する

### Requirement: Strategic Target Valuation

MUST: V1〜V4の戦術行動は、敵unit本体だけでなく、そのunitが運ぶ兵力と目前の占領による経済損失を含む戦略標的価値で攻撃対象と接近対象を評価しなければならない。

#### Scenario: 搭載済み輸送を空輸送より優先する

- **WHEN** 同距離・同HP・同unit種の敵輸送unitが複数あり、一方だけがcargoを搭載しているとき
- **THEN** 輸送unit本体のcostへ搭載cargoのunit costを加え、同じ期待損害なら搭載済み輸送を優先する

#### Scenario: 目前の拠点占領を阻止する

- **WHEN** 敵の占領可能unitが敵自身の所有ではない収入拠点上にいるとき
- **THEN** 当該unitの標的価値へその拠点の1ターン分の実収入を加える。敵自身の所有拠点上では加えない

#### Scenario: 輸送中cargoを独立標的として追跡しない

- **WHEN** 敵cargoが`Transporting`状態で盤外座標を保持しているとき
- **THEN** cargoを独立した接近対象から除外し、そのunit costを搭載元輸送unitの標的価値へ含める

### Requirement: Entity単位の一意な作戦所有権

MUST: AIは各自軍Entityを同時に1つの作戦ownerおよび1つの具体Squadだけへ所属させなければならない。作戦所有権の正本はEntityをキーとする単一registryとし、Squad、campaign、Roadmap、生産意図が独立した正本を持ってはならない。

#### Scenario: 別島作戦への重複混入を正規化する

- **WHEN** 同じEntityが異なる島を対象とする複数Squadへ混入したとき
- **THEN** 明示的なportfolio再計画、生産時意図、物理搭載の規則で1作戦だけを選び、他のSquadのmembers、cargo、delivered参照から同じ手番の行動選択前に除去する

#### Scenario: 作戦再計画で所有権を排他的に移管する

- **WHEN** portfolio再計画が既存Entityを別作戦の優先候補として選んだとき
- **THEN** 新作戦への登録と旧作戦の逆引きからの除去を原子的に行い、同じplanning passの下位候補には再割当しない

#### Scenario: 作戦終了時に所属Entityだけを解放する

- **WHEN** campaignが完了または撤回され、現行portfolioから消えたとき
- **THEN** 全Entityまたは全作戦を走査せず、作戦ownerの逆引きに属するEntityだけを解放する

#### Scenario: 行動候補ごとの重複走査を行わない

- **WHEN** 1手番のSquad計画と行動選択を行うとき
- **THEN** 一意性検証はplanning境界のO(U+R)正規化に限定し、以後のowner照会はEntityキーの平均O(1) lookupを使用する

### Requirement: 遊兵の有界再割当

MUST: V4は`Unassigned + Reserve`を遊兵として監査し、Unassignedをplanning境界で0件にし、Reserveを次の自軍手番までに通常作戦へ再割当しなければならない。

#### Scenario: Reserveを次手番の再割当入力に戻す

- **WHEN** 前の自軍手番末にReserveへ入った生存Entityが次の自軍手番を迎えたとき
- **THEN** Reserve Squadを外して現行Campaign、deployment、島portfolio、首都Forming、輸送波の順に再接続し、同じEntityを理由なく再びReserveへ残さない

#### Scenario: 未配備輸送を待つ既存占領兵を作戦へ保持する

- **WHEN** 海外作戦に既存の占領兵がいるが、同じ手番に適合する輸送役が存在しないとき
- **THEN** 占領兵を当該CampaignのTransport/Forming cargoへ接続し、Reserveへ落として同型占領兵を再生産しない

#### Scenario: 到着期限をsoft limitとして扱う

- **WHEN** 既存Entityの予想到着が予定手番を超えるが、合法に作戦へ到達でき、新規生産より早いか安いとき
- **THEN** 期限超過だけで候補から除外せず、遅延を予実差として記録して作戦へ割り当てる

#### Scenario: 生産時anchorから別作戦へ再配置する

- **WHEN** 生産時anchorへ照合済みのEntityを現在のportfolioまたは勝利Roadmapが別作戦へ明示移管したとき
- **THEN** 生産記録を照合完了にし、旧anchorで現在ownerを上書きせず、新作戦の具体SquadとRoadmap実績へ接続する

#### Scenario: Reserve期限超過を計測する

- **WHEN** EntityがReserveへ入った手番と次の自軍手番を監査するとき
- **THEN** 入った手番をage 0、次の自軍手番にもReserveならage 1として出力し、`age >= 1`を期限超過件数へ含める。行動済みでもReserveから除外しない

### Requirement: 敵配置に追従する手番内経路探索cache

MUST: V3/V4の行動探索は、同じplayerの自軍手番中に敵占有座標が不変である間だけターン距離結果を再利用し、経路の通行可否を変える敵配置または手番主体が変化した場合はcacheを破棄しなければならない。

#### Scenario: 味方unitだけが移動する

- **WHEN** 1unitの行動後も敵占有座標が変わらず、次の味方unitを評価するとき
- **THEN** 味方が経路通過を阻害しない移動ルールに基づき、同じ敵配置で計算済みのターン距離を再利用する

#### Scenario: 敵unitが撃破される

- **WHEN** 攻撃によって敵占有座標集合が変化し、次の味方unitを評価するとき
- **THEN** 古い敵を障害物として含むcacheを破棄し、現在盤面からターン距離を再計算する
