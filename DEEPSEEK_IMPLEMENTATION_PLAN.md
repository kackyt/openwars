# DEEPSEEK 実装計画: 標準経路への「撃破→進入」実行順序の導入

対象ブランチ: `codex/cooperative-ordering-standard-20261102`（基点 `f7abade` = 不合格試作の保存コミット）
作業日: 2026-11-02

---

## 1. 何がうまくいっていないのか（コードで確認した原因）

HANDOFF と `docs/verification/2026-11-02-codex/regional-execution-review.md` の未達は、計画が無いことではなく
**計画を1手番の行動順に変換する層が無いこと**で説明できる。確認した事実:

| 事実 | 根拠 |
| --- | --- |
| 手番は「1呼び出し = 1命令」。毎回、未行動の全ユニットを `(ActionPriority, score)` で横断比較して最良1手だけを出す | `engine/src/ai/runtime/turn.rs:89-223`, `engine/src/ai/tactics/scoring.rs:26`, `:55-67` |
| 標準経路（map_20 30x21=630、map_47 24x27=648 は小規模判定 196 を超える）の行動選択は `tactics/v2.rs` | `engine/src/ai/strategy_profile.rs` の `SMALL_MAP_MAX_CELLS`, `turn.rs:434` |
| 小規模経路にだけ「前衛より先に静止射撃を確定する」順序機構がある。標準経路には無い | `engine/src/ai/tactics/small_map/squad_executor.rs:256-267`, `:343-427` |
| 計画側の「排除・進入・占領・後続」をユニット1体の時間順序へ束ねる型が無い。地域状態は観測専用 | `capital_route/node_operation.rs:478-659`（観測）, `rolling_plan.rs` は購入比較用 |
| 間接射撃は移動した手番に撃てない（自走砲は「その場射撃」か「移動」の二択） | `engine/src/systems/combat.rs:518` |
| ダメージは決定論（乱数幅は固定シード0..10） | `engine/src/resources/mod.rs:49-52`, `combat.rs:522` |

map_20 先手 T20 の失敗は、**必要火力が3体分（歩兵12 + 重歩兵17 + 重自走砲35 = HP64）** だったのに、
3体が先に撃ち終えた結果、**進入できる4体目が残っていない**こと。射程や unlock 条件では解消しない。

不合格試作はこの順序層を作らず、代わりに次の3つの制約を足していた。これは順序の代用にならず、
進入T46の喪失と所有拠点27→24の悪化を招いたと読める（報告の記述との突き合わせ。個別の因果は未分離）:

1. `attack_priority` で地域外の不利交換を除外（`f7abade` の `tactics/v2.rs:736-773`）
2. 地域到達後に移動候補を `control_area` へハード絞り込み（`f7abade` の `tactics/v2.rs:314-345`）
3. 地域Control中の後退Waitを全面禁止（`f7abade` の `tactics/v2.rs:824-836`）

---

## 2. 方針（ユーザー決定）

- **主軸: 実行順序を新設する。** 点数・候補集合の調整ではなく、標準経路に「撃破担当の集合」と
  「進入役の予約」を1手番の順序として導入する。
- **基点: 試作品の上で修正する**（地域DAG・commitment・tactical の接続は流用し、上記3制約を差し戻す）。
- **検証: map_20 両手番・map_47 先手を同等に扱う。** map_47 後手は対象外のまま。

### 守る不変条件（No-Hack Principles）
- 特定マップ・座標・兵種名の分岐を追加しない。兵種能力はマスターデータから読む。
- `score += 定数` による誘導を追加しない。既存の優先度・採点式は変えない。
- 関数本文に `if is_extended()` を増やさない。差し替えは**手番ループの段**として外に置く。
- ユニットの損失自体を失敗条件にしない。

---

## 3. 実装

### 3-1. 残す変更（試作から採用）

- `engine/src/ai/tactics/campaign.rs` の DAG 指令投影（`campaign.rs:57-77`）。
  標準経路の V4 も `capital_route_tactical_target` / `capital_route_tactical_mission` を正本にする。
  `code_path::is_extended()` による分岐を**削除**する形なので、関数内分岐は増えない。
- `engine/src/ai/v4/capital_route/tactical.rs` の地域キャッシュ（`prepare_region_tactical_cache`, `:23-45`）。
- 地域目標・占領担当の一対一割当（`tactical.rs:367-437`）、入口選択（`:283-363`）。

### 3-2. 差し戻す変更（順序層で置き換える）

試作 `f7abade` の以下を削除し、`460e3d2` の挙動へ戻す。

| 対象 | 内容 |
| --- | --- |
| `tactics/v2.rs` `attack_priority` | 地域内外での戦略標的の絞り込み（`capital_route_attack_target_is_strategic` の呼び出し）を外し、配備目標 + 攻勢任務の既存分類へ戻す |
| `tactics/v2.rs` `best_action_for_unit` | `campaign.is_some()` 時の移動候補の `control_area` 絞り込みを外す |
| `tactics/v2.rs` `consider_wait` | `target.is_none()` のときの後退禁止分岐を外し、`is_none_or(..)` の既存規則へ戻す |

理由: いずれも「順序」を持たないまま行動を禁止する制約で、掃射できる敵を落とし、進入を失わせた。
順序層（3-3）が同じ目的を、禁止ではなく**実行の並べ方**で達成する。

### 3-3. 新規: 撃破担当と進入役の予約（`engine/src/ai/v4/fire_follow_order.rs`）

> 名称について: 既存の「島嶼 Assault（`AssaultCapital` / `IslandCampaign` の強襲パッケージ）」とは
> 別物である。島嶼 Assault の削除は既に棄却済み（`docs/07-project-management/plans/v4-production-ai-fix.md:285`）。
> ここで扱うのは**同一手番内の射撃→進入の順序**だけで、名前は既存資料の「砲撃→前進」に合わせる。

`FireFollowOrderRegistry`（Resource, player ごと・手番ごと）を新設する。値は以下だけを持つ。

```rust
pub(crate) struct FireFollowOrder {
    /// 排除対象の敵と、そのセル。
    target: Entity,
    cell: GridPosition,
    /// この敵を倒すために今手番で撃つ担当（合計期待ダメージ >= 敵HP）。
    fire_tasks: Vec<Entity>,
    /// 排除後にそのセルへ入る役。撃破担当とは重複させない。
    entry: Option<Entity>,
    /// この敵がまだ生きていて、未行動の撃破担当が残っているか（毎ステップ再判定）。
    active: bool,
}
```

構築手順（`fire_follow_order::observe`、手番開始後に1回 + 敵の死亡を観測した時に再構築）:

1. 対象セル = 現在の `CapitalRouteCommitment`（`phase == CapitalRouteExecutionPhase::Advance`）の
   `capture_targets` と `execution_target` のうち、**敵地上ユニットが占有しているセル**。
2. そのセルを撃てる自軍ユニットを、**現在位置からの静止射撃**で列挙する
   （`pruning::evaluate_attack_exchange(world, attacker, enemy, attacker_position=現位置, chart)` を再利用。
   期待ダメージは既存の交換予測をそのまま使い、新しい係数を足さない）。
3. 期待ダメージの降順（同値は Entity の安定ID順）に足し上げ、**合計が敵HP以上になる最小集合**を `fire_tasks` にする。
4. 進入役 `entry` = 次を満たす1体。撃破担当に含めない。
   - 直接戦闘 or 占領可能（`min_range <= 1`）
   - 敵を除いた占有で、今手番そのセルへ到達できる（既存の `calculate_reachable_tiles` を再利用）
   - まだ行動していない
   複数候補は「到達ターン距離 → 移動残コスト → 安定ID」で決める（既存の距離関数のみ使用）。
5. `fire_tasks` も `entry` も揃わない場合（1体で倒せない、進入役が居ない等）は**何もしない**。
   既存の採点に委ねる。新しい禁止・加点は入れない。

### 3-4. 手番ループへの接続（`engine/src/ai/runtime/turn.rs`, 標準経路のみ）

`decide_normal_action(world, &step, &skip)` の**直前**に段を1つ足す。

```rust
// 標準経路のV4だけ。撃破担当の射撃を先に確定し、進入役は撃破まで行動させない。
if step.is_v4 && step.tactics == TurnTactics::Standard
    && let Some((entity, command)) =
        crate::ai::v4::fire_follow_order::decide_fire_follow_action(world, step.player_id, &step.acted)
{
    return Some(issue_unit_command(world, entity, command));
}
```

`decide_fire_follow_action` の規則:

1. `active` な注文のうち、**未行動の撃破担当**が「現位置から対象を撃てる」なら、その射撃
   （`AiCommand::Attack`、移動なし）を返す。移動を伴う射撃は返さない（間接は移動後射撃不可のため）。
2. すべての `active` 注文で撃破担当が行動済み、または対象が消滅したら `active = false`。
   対象が消滅した時点で `entry` は通常行動へ戻る（同じ手番のうちに、まだ未行動なので進入できる）。
3. **進入役の抑止**は既存の `skip` 機構で行う。`decision_skip_entities` に
   「`active` な注文の `entry`」を加える。新しい待機命令・優先度は導入しない。
4. 撃破担当が今手番に誰も撃てない（行動済み・射程外・弾切れ）なら、注文を破棄して通常評価へ戻す。
   デッドロックを作らない（毎ステップ再構築するため、敵が死ねば自動解放される）。

配置上の注意:
- 段は `decide_normal_action` の外に置く。`tactics/v2.rs` の関数内へ経路分岐を足さない。
- 小規模経路（`TurnTactics::SmallMapSquads`）は既存の `try_stationary_support_fire_before_frontline`
  のままとし、この段は適用しない（`step.tactics == Standard` で限定）。

### 3-5. 観測（挙動を変えない）

- `wait_probe::stage` に `"fire_follow_order"` を追加（`engine/src/ai/wait_probe.rs`）。
- 新規イベント `ai_fire_follow_order`（既存 `invasion_events` と同形式）に、注文ごとの
  `target / cell / fire_tasks / entry / 発行された射撃 / 進入の成否` を出す。既定OFF
  （サーバ起動時の環境変数で有効化する。`wait_probe` と同じ方式）。

**計測スクリプトの前提確認（重要）**:
`scripts/review_regional_execution.py` は入力 JSON の `data["snapshots"]`（`before`/`after`/`events`/`routes`）を
読む（`:26`）。しかしこのキーを生成するスクリプトはリポジトリ内に存在しない（grep 一致は消費側のみ）。
`eval_matchup.py` が書くのは `strategic_history`（`properties`/`units`/`transport_squads`）であり、
`events`/`routes` を含まない（`:324-332`）。

そのため、**新しい順序機構を入れる前に**次を満たすこと:

1. `eval_matchup.py` を変更せず、`run_single_game(..., tool_caller=wrapper)` を包む
   **自前レコーダー** `scripts/record_fire_follow_snapshots.py` を新設する。
   ラッパーは `get_board_state` の前後と `simulate_ai_turn` の前後で盤面・イベント・
   `capital_routes` を保存し、`review_regional_execution.py` が読める `snapshots` 形式で書く。
2. 既存の `review_regional_execution.py` は**変更しない**（試作と同じ読取口を使い、数値を比較可能にする）。
3. `record_fire_follow_snapshots.py` は既存の `review_regional_execution.py` に
   合格する最小の `snapshots` を出力できることを、1局（map_20 先手 5T）で先に確認する。

### 3-6. テスト（先に固定盤面で赤→緑）

`engine/src/ai/v4/fire_follow_order/tests.rs` を新設。

1. **T20 再現**: 敵HP64に、期待12の歩兵・17の重歩兵・35の重自走砲が射程内。撃破担当3体が選ばれ、
   進入役の歩兵は `skip` に入る。3体が撃った後に敵が消え、進入役が同じ手番で対象セルへ入る。
2. **進入役が居ない場合**: 注文が作られず、従来の最良行動がそのまま選ばれる。
3. **撃破担当が行動済み**: 注文が破棄され、進入役が通常評価に戻る（待機で詰まらない）。
4. **1体で倒せる敵**: 撃破担当1体、進入役は別ユニット。進入役の行動が1手番残る。
5. **地域外の敵**: 注文対象にしない（地域外の有利な射撃は従来どおり）。3-2 の差し戻しとの整合。
6. **map_20 先手の進入T46が復活するか**を固定ケースで確認（試作で失った挙動の回帰）。

既存の地域構造テスト（`capital_route/tests.rs`）は流用し、3-2 の差し戻しで落ちる
「地域内で不利交換を撃たない」「地域外へ出ない」系の期待値だけを、実装に合わせて更新する
（自動 bless はしない。差分を読んで判断する）。

---

## 4. 実装したら何がどう改善することを期待するか

以下は**期待値であり、実測結果ではない**。合否は 5 節の観測項目で判定する。

### 4-1. 直接の期待（局地）

- **「撃破→進入」が同一手番で完結する。** 現状 map_20 先手は T20 時点で
  3体が撃って敵を消した後に進入役が居ない。導入後は、撃破担当と別の進入役が
  同じ手番に空いたセルへ入る。
  - 観測: `review_regional_execution.py` の「砲撃/攻撃による排除後に同手番で進入した事例数」。
    map_20 先手 6 件、map_20 後手 19 件、map_47 先手 6 件（60T時点）を基準線とし、増加を確認する。
  - 期待する形: 進入役 1 体が余るため、**進入件数が「撃破可能な敵の数」に近づく**。
    撃破そのものは既存の攻撃評価で起きているため、撃破数は増えず**進入だけが増える**のが正常。
- **前線の実位置が前へ出る。** 進入が成立すると、次手番の攻撃起点と占領対象が前へ移る。
  - 観測: 首都生産圏（敵首都から3マス, `PRODUCTION_RANGE`）への実進入の初手番。
    現状は map_20 両手番・map_47 先手とも 60T で「なし」。**T60までに1回以上**を期待する。

### 4-2. 狙いに対する期待（設計達成条件）

- 首都地域 `Advance` の指示ではなく、**地上戦闘部隊が生産圏セルに物理的に立つ**こと。
  - map_20 両手番・map_47 先手のいずれかで、**敵手番を1回越えて維持**された占位が1件以上。
- 生産圏内の敵生産施設の**占位または占領**が1件以上。占位は「敵手番を越えて維持」したものだけ数える。
- 所有拠点数が悪化しない。試作の map_20 先手 T60 = 24（直前版 27）を**上回る**こと。

### 4-3. 期待しないこと（過大評価の防止）

- 勝率・決着ターンの改善は**この段階では主張しない**。map_20 は 60T 時間切れが基本で、
  決着には生産口封鎖の持続が別途必要（`map42-decisive-victory-mechanism` の知見と同型）。
- map_47 先手の決着ターン短縮を成功根拠にしない（敵首都所有者が敵のまま短縮する例が既にある）。
- 撃破数・損失数の増減は合否に使わない（ユーザー指示: 損失自体を問題としない）。

### 4-4. 予想される副作用と監視点

- 撃破担当が移動せず撃つ分、**前進が1手番遅れる局面**が出る。特に敵が1体しか居ない序盤。
  - 監視: map_40・map_6 の 30T 再現性（ゴールデン）。差分は移動順・展開先に留まるか審査する。
- 進入役の抑止で**待機が増える**可能性。デッドロック防止（3-4-4）が効いているかを
  「撃破担当が行動済みなのに entry が skip されたまま」の件数0で確認する。
- 小規模経路は非適用。map_1・map_2 のゴールデンは変化しないことを確認する。

---

## 5. 検証手順（3ケースを同等に）

作業ディレクトリはリポジトリルート。評価器の既定バイナリは release なので明示する。

```bash
cargo build --offline --release -p mcp-server
cargo test --offline -p engine --lib
cargo clippy --offline --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

### Gate 1: 局地の順序（固定ケース）
- map_20 先手 seed1, 22T。`OPENWARS_WAIT_PROBE=1` + `ai_fire_follow_order` イベントで
  T20 の (20,8) 相当の局面を確認する。期待: 撃破担当3体 → 進入役1体が同手番で進入。
- 失敗条件: 注文が作られない / 進入役が skip されたまま敵が死ぬ / 撃破担当が撃たない。

### Gate 2: 3ケース 60T（seed 1）
ベースライン（`f7abade` から 3-2 の差し戻しだけを適用した版）と現行版を、同じ条件で並べて取る。

```bash
# 変種は必ず別名の実体を渡す（target/release を直接渡すと再ビルドで混ざる）
python scripts/run_matchup_sweep.py \
  --variant base=.tmp/order/base/mcp-server.exe \
  --variant current=.tmp/order/current/mcp-server.exe \
  --map map_20,map_47 --seeds 1 --repeats 1 --jobs 4 \
  --env current:OPENWARS_FIRE_FOLLOW_TRACE=1 \
  --out-dir .tmp/order-sweep
```
- `--env` は `NAME:KEY=VALUE` 形式（`run_matchup_sweep.py:186`）。`ai_fire_follow_order` の出力はこの変種だけに付ける。
- map_20 は両手番、map_47 は先手のみ（`run_matchup_sweep.py` は全手番順を回すため、
  map_47 後手の結果は**判定に使わない**。対象外であることを集計時に明記する）。
- 各ケースで 4-1 / 4-2 の観測項目を取り、ベースラインと突き合わせる。
- `run_matchup_sweep.py` を使わない場合は `eval_matchup.py --player-order both` を map_20 に、
  `--player-order as-given` を map_47 に、`--json-output` を1ケース1ファイルで回す。

### Gate 3: 副作用
- map_40 両手番 seed1〜3、map_6 両手番 30T 再現性、map_1/map_2 ゴールデン。
- `cargo test --workspace`（golden は自動 bless しない。差分を審査してから更新）。

### 判定
- Gate 1 が通らない限り Gate 2 を合格扱いにしない。
- Gate 2 は「進入件数」「生産圏実進入の初手番」「維持された封鎖」「所有拠点」を**別々に**報告する。
  合成点・加重和は作らない。
- 3ケース中1つでも所有拠点が悪化したら、改善と報告しない（試作と同じ扱い）。

### 既に否定済みの範囲（再着手しない）
サブエージェントの調査で確認した、この方向と隣接して**すでに棄却された**案。着手前に読む。

- `docs/architecture/ai-redesign-v2-report.md:105-114`: 戦略変更 H1〜H4（占領前線起点、戦闘枠から輸送兵種除外、
  未割当占領兵の計上、物件収入の振れ幅加点）は全て不採用。
- `docs/verification/2026-11-02-codex/regional-execution-review.md:116,122,128,138,164`: 地域制圧の中間版6種
  （作戦座標を方面ID扱い、到達順のみ、防衛余剰配分のみ、橋の射撃支援を通過枠と同居、フェリー射撃接続後の直接進入）。
  いずれも「指示は出るが実進入・封鎖なし」。
- `docs/architecture/ai-redesign-v3-stage6b-balance-validation.json`（`:7,38,62-66,90`）: 直接射点を一律維持する
  間接混成ゲート。既存 map32 契約違反で棄却。
- `docs/architecture/ai-redesign-v3-stage6b-d5-validation.json`（`:23-28`）: 取消命令の意図解放。28条件で発火0件。
- `docs/07-project-management/plans/v4-production-ai-fix.md:285`: 進行中 Assault の削除（3-3 の命名注意の根拠）。
- `docs/07-project-management/plans/v4-v200-small-map-expansion-analysis.md:90`: ETA を無視した占領兵増員。
- `docs/07-project-management/plans/v4-fix-wait-scoring.md:138`: 敵占有を壁とみなす距離。
- `HANDOFF.md:92`: 後退拒否の全解除とスコア定数の追加は明示的に回避。

**今回の案がこれらと違う点**: 点数・禁止・候補集合を変えず、**未行動ユニットの実行順に注文を挟む**。
そのため、上記の「指示は出るが実進入が起きない」失敗を、指示の追加ではなく順序で解こうとする。

---

## 6. 実装順序と中断条件

1. 3-2 の差し戻しのみ実施 → ベースラインが `460e3d2` と一致することを Gate 1/ゴールデンで確認。
   （ここで一致しなければ、試作の他の変更が混ざっている。先に切り分ける）
2. 3-3 + 3-6 の単体テスト（赤→緑）。この時点で Gate 2 は回さない。
3. 3-4 の接続 + Gate 1。局地の順序が変わることを確認。
4. Gate 2（seed1, 3ケース）→ 4-1/4-2 を報告。
5. 設計達成条件を満たした場合だけ Gate 3 と 3seed へ進む。

**中断条件**: Gate 1 で「進入役が skip されたまま敵が死ぬ」または「撃破担当が撃たない」が
1 ケースでも出たら、順序層の設計をやり直す。点数や閾値の調整で通そうとしない。

**前提作業（3-3 より先）**: `scripts/record_fire_follow_snapshots.py` を作り、
`review_regional_execution.py` が読める `snapshots` を1局分出力できることを確認する（3-5）。
これが無いと、4-1/4-2 の観測項目を試作と同じ尺度で測れない。

---

## 7. 触らない場所

- `tactics/v2.rs` の採点式（`tile_score` / `assess_attack` / `approach_bonus`）。
- `rolling_plan.rs` の購入比較と `indirect_upgrade_preserves_direct_plan`。
- `small_map.rs` / `squad_executor.rs`（小規模経路）。
- `victory_roadmap` の首都作戦スコープのゲート（`is_extended`）。
