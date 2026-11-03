# Codex Implementation Plan: 砲台 (`Artillery`)・補給輸送車 (`SupplyTruck`) の過剰生産・右往左往・前線閉塞の根本解消と敵首都・拠点突入の完遂

> **作成日**: 2026-11-01  
> **対象ブランチ**: `claude/dazzling-lovelace-if6j7f` (`HEAD: 85d46b4`)  
> **対象課題**: `map_20`（先攻・後攻とも 60T 時間切れ判定勝ち）および `map_47`（先攻で T18 に 10 倍以上の戦力差をつけながら T48〜T56 まで決着遅延）において、砲台 (`Artillery`) と補給輸送車 (`SupplyTruck`) が過剰生産されて前線・橋・隘路を占拠し、乗降 (`Load`/`Drop`) や東西移動を繰り返して右往左往することで、戦車・歩兵が敵首都および敵工場・拠点に攻め込めない問題。

---

## 1. 定量的な事象とログ証拠（Pre-Investigation 結果）

### 1.1 `map_20`（先攻 `V4-V200` / 後攻 `V200-V4` とも全 6 戦が 60T 時間切れ）
- **先攻 (`map_20_V4-V200_r0`, seed 1)**:
  - T60 時点で V4 戦力価値 **1,134,800** vs V200 **113,300**（約 10 倍）、拠点 **29 vs 17** と圧倒的優勢だが 60T 時間切れ。
  - T60 の行動内訳: `Attack(9)`, **`Wait(106)`**, `Drop(1)`, `Load(1)`。106 ユニットが待機渋滞。
  - 延べ行動ユニット構成: `Infantry: 200`, **`Artillery: 39`, `SupplyTruck: 38`（計 77）** に対し、直接戦闘車両は **`Tank: 12`, `MdTank: 7`（計 19）** のみ。
  - **敵首都直前の閉塞と右往左往**: P2 首都 `(21,16)` の直前は幅 1〜2 マスの隘路（`x=16..22, y=16..19`、都市 `(16,16), (16,17), (17,16)`）。この隘路に占領能力のない `Artillery` と `SupplyTruck` が 10 台以上詰まり、さらに `(17,3)` 等の同一マスで T40 `Drop` → T41 `Load` → T42 `Drop` → T43 `Load` → T44 `Drop` と無限に乗降を繰り返し、後続 100 ユニット超の進軍を完全に塞いでいる。
- **後攻 (`map_20_V200-V4_r0`, seed 1)**:
  - T60 時点で V4 戦力価値 **1,090,200** vs V200 **194,100**（約 5.6 倍）、行動内訳は **`Wait(110)`**, `Supply(3)`, `Attack(9)`。先攻と全く同じ構造で P1 首都 `(8,3)` 手前の北西隘路・橋で砲台と補給輸送車が渋滞し 60T 時間切れ。

### 1.2 `map_47`（先攻 `V4-V200`、seed 1: 48T, seed 2: 56T, seed 3: 55T）
- **戦力比と停滞**: T18 時点で V4 戦力価値 **905,960** vs V200 **71,527**（約 12.6 倍、拠点 28 vs 12）と事実上決着しているが、そこから 30〜38 ターンも敵首都 `(8,10)`・敵工場 `(7,10)` に踏み込めない。
- **異常な砲台＋補給輸送車の無限ラチェット生産（全 seed 共通）**:
  - **seed 1**: `Infantry: 135`, **`Artillery: 64`, `SupplyTruck: 68`** vs `Tank: 16`, `MdTank: 1`, `TankZ: 7`
  - **seed 2**: `Infantry: 134`, **`Artillery: 88`, `SupplyTruck: 90`** vs **`Tank: 0`, `MdTank: 0`**, `TankZ: 3`, `HeavySpGun: 5`
  - **seed 3**: `Infantry: 151`, **`Artillery: 80`, `SupplyTruck: 85`** vs `Tank: 2`, `MdTank: 3`, `TankZ: 7`, `HeavySpGun: 5`
- **非武装の `SupplyTruck` が `Attack` 小隊として前線・橋へ突撃**:
  - `strategic_history` (`audit`) にて、`squad_id: 34, 40, 48, 55, 64, 97, 136` 等が `member: [SupplyTruck, SupplyTruck, ...]` の構成で **`mission_type: Attack`**（目標: 敵拠点 `(4,11), (7,7), (8,17)` や敵首都 `(8,10)`）を付与され、移動力 5 で橋 (`y=13, x=11..14`) や西側隘路へ突っ込み、戦車と歩兵の進路を物理的に塞いでいる。
- **砲台フェリーの東西往復（右往左往）**:
  - 輸送された砲台が北部の山地・都市群 (`y=1..7, x=4..20`) や東側道路 (`x=13..20`) で毎ターン `Drop` と `Load` を繰り返すばかりで、敵工場 `(7,10)`・敵首都 `(8,10)` への射撃位置 `(10,9), (9,11)` には T48 まで到達しない。

---

## 2. 厳守すべき制約（No-Hack Principles & Project Conventions）

実装にあたっては `.agents/skills/openwars-ai-improvement-cycle/references/no-hack-principles.md` および `AGENTS.md` / `GEMINI.md` の規約を厳守すること。

1. **スコアハック・マジックナンバーの禁止**:
   - `score += 3000` や `exposure_penalty / 4` のような恣意的な定数加算・割り算で行動を誘導してはならない。
   - 特定の `map_id`（`map_20`, `map_47` 等）や特定座標のハードコードは厳禁。
   - `Artillery` のみを無条件に生産禁止にするような局所パッチではなく、**「既存戦力の認識漏れ」「非戦闘ユニットの小隊編成混入」「ターン内行動順序」「窄路での RollingPlan 前線幅モデル」「地形距離に基づくルート目標判定」** という構造的原因を修正すること。
2. **コーディング規約**:
   - ロジックの意図がわかるように日本語のコメントを記述すること。
   - 以下の全チェックがエラー・警告なしで通ること:
     - `cargo fmt --all -- --check`
     - `cargo clippy --all-targets --all-features -- -D warnings`
     - `cargo test`

---

## 3. 根本原因の連鎖構造（5つの構造的バグ）

調査により、以下の 5 つのバグが噛み合って「砲台・補給輸送車の過剰生産」と「前線での右往左往・渋滞」を引き起こしていることが特定された。

1. **【原因1：小隊編成バグ】非武装の `SupplyTruck` が `free.combat` に分類され、`Attack` / `Defense` 小隊に編入されて前線へ突撃する**
2. **【原因2：生産計画バグ】降車 (`Drop`) 直後の砲台やルート作戦所属の砲台が `Operation` の `existing_units` から脱落し、毎ターン「砲台 0 門」と誤認して砲台と補給輸送車を 1:1 で無限ラチェット生産する**
3. **【原因3：ターン内行動順序と乗降チャタリング】`decide_artillery_ferry` の全ステップが `decide_normal_action` より先に実行されるため、輸送車が戦車・歩兵より先に橋・隘路を塞ぎ、また展開済み砲台が射撃対象の一時不在で即座に再乗車 (`Load`) して 2 ターン周期の `Load`/`Drop` ループに陥る**
4. **【原因4：RollingPlan 評価の歪み】隘路マップ（`front_min_cut == 1`）で直接戦闘車両の同時攻撃数が `1` に頭打ちされる一方、牽引砲台は「保護歩兵 1 体でも前線スクリーン成立（被撃破 0）」と判定されて 20〜50 門同時射撃可能と過大評価され、さらに降車 (`Drop`) に要する 1 ターン遅延が ETA に含まれていない**
5. **【原因5：ルート目標・DAG 遷移ロック】`beyond_defended_route` が地形（海・入り江）を無視した直線ヘックス距離 (`map.distance`) で判定するため `map_47` の敵工場 `(7,10)` が攻撃対象から除外され、また `map_20` では敵首都直前ノードの都市 1 マスが敵歩兵に競合されるだけで敵首都ノードが永久に `Locked` のままになる**

---

## 4. 具体的な改修仕様（Task 1 〜 Task 5）

以下にファイル・関数・行番号単位での改修仕様を示す。

---

### Task 1: 非武装輸送ユニット（`SupplyTruck` 等）の戦闘小隊混入防止と待機制御

#### 対象ファイル 1-A: `engine/src/ai/squad/formation.rs`
- **対象箇所**: `FreeUnits::collect`（L414〜428 付近）
- **現状の問題**:
  `island_campaign.rs` L365 (`is_campaign_support_unit`) では `SupplyTruck` を戦闘ユニットから除外しているが、`formation.rs` の `FreeUnits::collect` では `is_transport` が `TransportHelicopter | Lander` のみになっており、`SupplyTruck`（および攻撃力を持たない支援車両）が `else { free.combat.push(...) }` に落ちている。
  その結果、`form_attack_squads`（L1389）や `absorb_surplus_combat_units`（L1423）が `SupplyTruck` を `MissionType::Attack` / `Defense` 小隊に編入し、小隊定員 (`MAX_SQUAD_SIZE = 3`) を食い潰すうえ、`SupplyTruck` 単独の `Attack` 小隊を大量生成して敵首都や橋へ突撃させている。
- **修正内容**:
  1. `FreeUnits::collect` において、砲台牽引車・非武装支援ユニット（`crate::ai::v4::artillery_ferry::is_artillery_carrier(&stats)` または `stats.max_range == 0 && !stats.can_capture && !is_transport`）を `free.combat` に入れないように除外する。
  2. 念のため `absorb_surplus_combat_units`（L1423 付近）および `form_attack_squads`（L1389 付近）でも、`stats.max_range == 0` の非武装ユニットが `Attack` / `Defense` 小隊に編入されないようガードを入れる。

```rust
// engine/src/ai/squad/formation.rs (FreeUnits::collect 内の分岐イメージ)
let is_transport = matches!(
    unit.unit_type,
    UnitType::TransportHelicopter | UnitType::Lander
);
let is_unarmed_support = crate::ai::v4::artillery_ferry::is_artillery_carrier(&stats)
    || (stats.max_range == 0 && !stats.can_capture && !is_transport);
if is_transport {
    free.transports.push((entity, unit.position, stats.clone()));
} else if is_unarmed_support {
    // 補給輸送車などの非武装支援車両は戦闘小隊（Attack/Defense）に編入しない
} else if stats.can_capture {
    free.infantry.push((entity, unit.position, stats.clone()));
} else {
    free.combat.push((entity, unit.position, stats.clone()));
}
```

#### 対象ファイル 1-B: `engine/src/ai/v4/artillery_ferry.rs`
- **対象箇所**: `ArtilleryFerryPlanner::hold`（L1423〜1438 付近）
- **現状の問題**:
  `hold` は `is_ferried_gun(&unit.stats) || self.tends_engaged_gun(unit)` のみを `AiCommand::Wait` にし、担当砲台を持たない余剰の空 `SupplyTruck` をスルーして `decide_normal_action`（`tactics/v2.rs`）に流している。そのため余剰の `SupplyTruck` が通常戦術 AI で前線へ前進し、橋や隘路を塞ぐ。
- **修正内容**:
  - `vacate_facility` の後に実行される `hold` において、まだ行動していない空の砲台牽引車（`is_artillery_carrier(&unit.stats) && unit.loaded_Cargo.is_empty()`、かつ自軍生産施設上でないもの）も `artillery_ferry` 側で管理対象として `AiCommand::Wait` を返し、`decide_normal_action` にフォールスルーして前線へ暴走しないようにする（自軍生産施設上の場合は `vacate_facility` が先に退避させる）。
  - ただし、隣接味方ユニットへの補給 (`Supply`) が可能な場合は `supply` ステップで既に処理されているため、`hold` 時点で残っている空の牽引車は安全に待機させてよい。

---

### Task 2: 生産計画における既存砲台の `Operation` 紐付け欠落修正と輸送車ラチェット防止

#### 対象ファイル 2-A: `engine/src/ai/v4/mission_input.rs`
- **対象箇所**: `combat_assignments`（L52〜154）
- **現状の問題**:
  1. Part 1（L52〜92）は `MissionType::Attack | MissionType::Defense` の小隊メンバーのみを走査する。ターン中に `decide_artillery_ferry::deliver` で `Drop` されたばかりの砲台は、ターン開始時には `Transporting` だったため `AdoptedTurnPortfolio` に載っておらず、`reconcile_v4_end_turn_reserves` で `MissionType::Reserve` に入れられるため Part 1 から漏れる。
  2. Part 2（L108〜153）は現在 `Transporting` コンポーネントを持つ被輸送砲台のみを走査するため、ターン中に `Drop` された砲台は Part 2 からも漏れる。
  3. さらに、Part 1 で拾われた展開済み砲台であっても、その `squad.target_position` は `firing_position`（前線から 3〜6 マス後方の空き地）や `front`（空き地）になっているため、後続の `assign_committed_combat` でどの `Operation` の `anchor` / `objective_properties` / `reachable_threats` とも一致せず捨てられる。
- **修正内容**:
  - `combat_assignments` において、自軍の牽引対象砲台（`is_ferried_gun(&stats)`）およびルート作戦メンバー（`squad_state.route_operation_members`）については、小隊が `Reserve` であっても、また `Transporting` から降車直後であっても漏れなく収集する。
  - 砲台または `route_operation_members` の `objective` 座標を決定する際、`squad.target_position` が空き地（`firing_position` や `front`）であるために `Operation` とマッチしない問題を防ぐため、以下の優先順で正規化した目標座標（または所属ルート目標）を持たせるか、2-B のマッチングを拡張する:
    1. `squad_state.route_operation_members.get(&entity)` または輸送車の `assigned_attack_route` に対応する `RouteOrder` の `key_point`
    2. 最寄りの自軍作戦目標（未占領拠点・敵脅威・`defend_points`）

#### 対象ファイル 2-B: `engine/src/ai/v4/production_pipeline/mod.rs`
- **対象箇所**: `assign_committed_combat`（L164〜217）および `plan_v4_production_with_diagnostics`（L48〜58）
- **現状の問題**:
  `assign_committed_combat`（L184〜200）は以下の完全一致条件でしか既存ユニットを `Operation` に紐付けていない:
  ```rust
  operation.anchor == target
      || operation.objective_properties.contains(&target)
      || operation.reachable_threats.iter().any(|threat| threat.position == target)
  ```
  この条件では、`target` が既に自軍占領済みの `key_point`（橋や前線都市）である場合や、砲台の `firing_position`（射程 3〜6 の後方タイル）、`staging` / `front` タイルである場合に **すべて不一致 (`None`) となり、既存の砲台・戦闘ユニットが `Operation.existing_units` から消滅する**。これが原因で `RollingPlan` が毎ターン「この作戦には砲台が 0 門しかない」と誤認し、無限に砲台を買い続ける。
- **修正内容**:
  1. `assign_committed_combat` で上記の完全一致で見つからない場合、同じ島（`operation.island_id == target_island`）の `Operation` のうち、以下を満たす最も近い `Operation` にフォールバック紐付けする:
     - `is_ferried_gun(&assignment.stats)` の場合: `target`（または現在位置 `assignment.position`）から `operation.anchor` / `operation.objective_properties` / `operation.reachable_threats` のいずれかまでのヘックス距離が砲台の射程 + 輸送圏内（または同一島の最寄り `Operation`）にある `Operation` に紐付ける。
     - 一般戦闘ユニットの場合: `target` が既に自軍拠点（占領済み `key_point` 等）や `front`/`staging` 空き地であっても、同一島内で `operation.anchor` または `operation.objective_properties` が `target` に最も近い `Operation` に紐付ける。
  2. `plan_v4_production_with_diagnostics`（L48〜58）において、`allocate_artillery_carriers` が `allocate_operation_slots` の**前**（L50）と**後**（L55）の 2 回呼ばれており、L50 の呼び出しが作戦スロット（戦車や歩兵の生産枠・資金）を先取りして奪っている。
     - 先行の `allocate_artillery_carriers`（L50）を削除または「現在すでにマップ上に存在し、同一島内に空きキャリアが 1 台もいない未積載・非交戦砲台に対する最小限のキャリア補充」に限定し、原則として `allocate_operation_slots`（戦車・歩兵等の作戦枠）を優先させる。

#### 対象ファイル 2-C: `engine/src/ai/v4/production_pipeline/artillery_carriers.rs`
- **対象箇所**: `allocate_artillery_carriers`（L48〜98）
- **現状の問題**:
  島内の全砲台数 `gun_count` と全輸送車数 `carriers.len()` を単純比較して `wanted = gun_count.saturating_sub(carriers.len() + new_carriers)` で 1:1 生産しているため、既に射撃陣地に展開して交戦中・防衛中の砲台に対してもすべて 1:1 で `SupplyTruck` を生産し続け、60〜90 台の輸送車でマップを埋め尽くしている。
- **修正内容**:
  - 既に有効な射撃位置に展開済み（敵または目標拠点が射程内にあり移動不要）の砲台に対しては、弾薬・燃料が欠乏に近づいている場合や既存の随伴キャリアでカバーできない場合を除き、新規の輸送車生産要求 (`gun_count`) から除外するか、実際に輸送を必要としている砲台（生産直後・後方に孤立している砲台 + 今ターン新規生産される砲台）の数に基づいて `wanted` を算出する。

#### 対象ファイル 2-D: `engine/src/ai/squad.rs`
- **対象箇所**: `reconnect_current_v4_operations`（L3778〜3843）
- **現状の問題**:
  `reconcile_v4_end_turn_reserves` から呼ばれる `reconnect_current_v4_operations` において、`local` の `combat_entities` を `repair_entities`（未所属ユニット）のみにフィルタした状態で `prepare_campaign_local_assignment_scoped` を呼んでいるため、内部で `route_operations::record_membership` や `responsibilities` が「`repair_entities` しか戦闘ユニットがいない」前提で再計算され、既存メンバーのルート所属や役割評価が歪む。
- **修正内容**:
  - `reconnect_current_v4_operations` では、ルート全体の既存メンバーシップ（`squad_state.route_operation_members` 等）を破壊しないよう保護するか、全生存戦闘ユニットをコンテキストとして渡した上で `repair_entities` のみを再編入する。

---

### Task 3: ターン内行動順序の分割（戦車・歩兵の先行進軍）と砲台フェリーの乗降チャタリング防止

#### 対象ファイル 3-A: `engine/src/ai/runtime/turn.rs` & `engine/src/ai/v4/artillery_ferry.rs`
- **対象箇所**:
  - `engine/src/ai/runtime/turn.rs`: `execute_v4_unit_actions_Recursive`（L140〜181）
  - `engine/src/ai/v4/artillery_ferry.rs`: `decide_artillery_ferry`（L162〜180）
- **現状の問題**:
  `turn.rs` L140 で `decide_artillery_ferry` の全 8 ステップ（`supply` → `fire` → `deliver` → `board` → `meet` → `tend` → `vacate_facility` → `hold`）が **`decide_normal_action`（L179：戦車や歩兵の移動・攻撃・占領）より先に実行されている**。
  これにより以下の 2 つの致命的現象が起きる:
  1. **橋・隘路の先回り閉塞**: `deliver` / `meet` / `tend` で移動力 5 の `SupplyTruck` が戦車・歩兵より先に橋や幅 1 マスの道路へ進入して停止し、そのターンに戦車・歩兵が前進できなくなる。
  2. **前線スクリーン未形成による後方 Drop**: 戦車が前進する前に `deliver` が走るため、`can_safely_emplace`（L584〜594）の `screened` 判定が「前方に味方直接戦闘ユニットがいない」と判定して前進展開を拒否し、後方で足踏みまたは降車させる。
- **修正内容**:
  `decide_artillery_ferry` を **Pre-Normal（通常行動前）** と **Post-Normal（通常行動後）** の 2 フェーズに分割する。
  1. **`decide_artillery_ferry_pre`（`decide_normal_action` の前に実行）**:
     - `vacate_facility()`: 自軍生産施設（工場等）の上にいる砲台・輸送車を即座に退避させ、施設や通路を空ける。
     - `supply()`: 既に隣接している味方ユニットへの補給を実行する（移動を伴わない、または最小限の補給）。
     - `fire()`: 既に展開済みの砲台による砲撃を実行する（戦車・歩兵が突撃する前に敵を削るため、砲撃は必ず先に行う）。
  2. **`decide_normal_action`（戦車・歩兵・戦闘車両の移動・攻撃・占領）**:
     - 砲撃で削れた敵に対し、戦車と歩兵が橋・隘路を通って前進・撃破・占領を行い、前線スクリーン (`screen_units`) を押し上げる。
  3. **`decide_artillery_ferry_post`（`decide_normal_action` の後に実行、または `decide_normal_action` の冒頭で砲台・輸送車をスキップして後段で実行）**:
     - `deliver()` / `board()` / `meet()` / `tend()` / `hold()`: 戦車・歩兵が前進し終えて開いた道路・更新された前線スクリーン (`screened`) に基づいて、砲台の降車展開・回収乗車・合流・随伴・待機を実行する。
     - ※実装上の注意点: `execute_v4_unit_actions_Recursive` は 1 アクションごとに再帰呼び出しされる構造になっているため、`decide_normal_action` 側で `is_ferried_gun(&stats) || is_artillery_carrier(&stats)` のユニットが通常戦術 (`tactics/v2.rs`) に奪われないよう、`decide_artillery_ferry_pre` → `decide_normal_action`（牽引砲台と輸送車はスキップ） → `decide_artillery_ferry_post` の順で評価すれば、1 アクションごとの再帰ループと完全に整合する！

```rust
// engine/src/ai/runtime/turn.rs 内の呼び出し順序イメージ
if matches!(profile, CommanderProfile::V4) {
    if let Some((entity, cmd)) = crate::ai::v4::artillery_ferry::decide_artillery_ferry_pre(
        world,
        current_faction,
        &ai_config,
        &map_clone,
        &registry_clone,
        &spatial_index,
    ) {
        if issue_command(world, entity, cmd) {
            return execute_v4_unit_actions_Recursive(world, current_faction, step_count + 1);
        }
    }
}

// decide_normal_action 内（またはその直前フィルタ）では、V4 の場合 is_ferried_gun / is_artillery_carrier をスキップさせ、
// 通常戦闘ユニット・歩兵がすべて行動し終えた後に decide_artillery_ferry_post を実行する。
```

#### 対象ファイル 3-B: `engine/src/ai/v4/artillery_ferry.rs`
- **対象箇所**: `idle_guns` / `has_better_place`（L1154〜1199）および `deliver`（L271〜345）
- **現状の問題**:
  `idle_guns`（L1154〜1177）は、展開済み砲台にそのターンの射撃対象がない（`best_shot.is_none()`）と、たとえ既に作戦ゾーン内 (`in_zone == true`) にいても `has_better_place(gun)` が `true` なら即座に回収対象にし、`board` で隣接する `SupplyTruck` に乗車 (`Load`) させる。
  ところが `has_better_place`（L1179〜1199）は:
  - `self.sites(gun, current, FAR_SITE_SEARCH_TURNS)` に `site.cell != current` の候補が 1 つでもある、または
  - `self.map.distance(current, *home) > 2`
  という極めて緩い条件で `true` を返す。
  その結果、前方の候補地がまだ `can_safely_emplace`（スクリーン条件）を満たしていない段階でも一旦 `Load` してしまい、次ターンの `deliver` では前方で降ろせないため**元のマス（または 1 マス隣）に再び `Drop` する**という 2 ターン周期の無限 `Load` ↔ `Drop` チャタリング（`map_20` の `(17,3)` や `map_47` の北部山地）が発生している。
- **修正内容**:
  1. **再乗車 (`board` / `idle_guns`) のヒステリシス（チャタリング防止）**:
     - 既に `in_zone` である展開済み砲台については、単に「別マスがある (`site.cell != current`)」だけで乗車させない。
     - 再乗車 (`idle_guns` への追加) を許可するのは以下のいずれかの場合に限定する:
       a. `!in_zone`（作戦目標の移動によりゾーン外に取り残された）かつ、現在位置から射程内に敵も未占領目標拠点も存在しない場合。
       b. `in_zone` であっても、現在位置から目標拠点（または最寄り敵）までの距離が砲台の `max_range` を大きく超えており（例: `distance > max_range + 2`）、かつキャリアが**現時点で実際に到達・安全降車可能 (`can_safely_emplace`) な、目標により近い（少なくとも 3 ヘックス以上前進できる）展開候補地**を保持している場合。
  2. **狭隘路（チョークポイント）での降車 (`Drop`) 禁止**:
     - `can_safely_emplace` または `sites` において、味方の進軍経路上の幅 1 マスの橋・隘路タイル（周囲の通行可能隣接タイルが 2 以下で迂回路がないボトルネック、または未占領の敵拠点・首都に隣接する突撃路）には砲台を `Drop` しないようにし、後続の戦車・歩兵の通過および拠点占領を妨害させない。

---

### Task 4: `RollingPlan` における牽引砲台の降車遅延（ETA）と隘路スクリーン条件の適正化

#### 対象ファイル 4: `engine/src/ai/v4/rolling_plan.rs`
- **対象箇所**:
  - `emplacement_site`（L533〜608）
  - `friendly_attacks`（L1396〜1416）
  - `enemy_attacks`（L1465〜1489）
- **現状の問題**:
  1. **降車 (`Drop`) ターンの欠落**: `emplacement_site`（L556〜583）で牽引砲台の到着ターン `travel` を `cost.div_ceil(movement).max(1)` で計算しているが、実際にはキャリアが移動して `Drop` したターンは砲台に `ActionCompleted` が付与されるため、**砲台が最初に射撃できるのは降車の翌ターン（`+ 1` ターン後）**である。この 1 ターン遅延が無視されているため、自走できる戦車に対して牽引砲台の即効性が不当に高く見積もられている。
  2. **保護歩兵 1 体による無限の無敵スクリーン判定**: `friendly_attacks`（L1405〜1416）および `enemy_attacks`（L1465〜1470）において、`ground_chokepoint_width == 1` のマップでは `living_ground_direct_count`（この中には占領用の保護歩兵 `protected_units` 1 体も含まれる）が `1` あるだけで `screen_units >= ground_chokepoint_width`（スクリーン成立）と判定され、**戦車が 0 台でも砲台 20〜50 門が被ダメージ 0 (`expected_loss = 0`) で毎ターン一方的に全門斉射できる**とシミュレーションされる（`map_47` seed 2 で `Tank: 0, MdTank: 0, Artillery: 88` になる直接原因）。
- **修正内容**:
  1. `emplacement_site` において、キャリア牽引が必要な砲台（`is_ferried_gun(&lane.stats)` かつ現在未展開または新規生産）の有効射撃開始までのターン数 `travel` に降車展開コスト `+ 1` ターンを加算する。
  2. `friendly_attacks` および `enemy_attacks` のスクリーン判定において、間接攻撃ユニット群を無傷で守る `ground_screen_holds` / `screen_units` のカウントには、脆い占領用保護歩兵 (`protected_units`) ではなく**非保護の直接戦闘ユニット（戦車等の実戦闘前衛）**を要求する（または間接ユニット数が直接戦闘前衛数の一定倍率を超える分はスクリーンで守り切れず露出するモデルにする）。これにより、前衛の戦車が 0 台のまま砲台だけを 80 門買うような非現実的なポートフォリオが自然に棄却される。

---

### Task 5: 地形考慮の `beyond_defended_route` 判定と敵首都直前ノードのロック解除

#### 対象ファイル 5-A: `engine/src/ai/squad/objective_allocation.rs` & `engine/src/ai/squad/route_operations.rs`
- **対象箇所**:
  - `engine/src/ai/squad/objective_allocation.rs`: `beyond_defended_route`（L629〜647）
  - `engine/src/ai/squad/route_operations.rs`: `advanced_offensive_target`（L515〜525）
- **現状の問題**:
  `beyond_defended_route(map, target, orders)` は、ターゲット拠点 `target` が防衛ルート（`Hold` / `Contain`）の `key_point` の先にあるかどうかを**直線ヘックス距離 (`map.distance`)** だけで比較している:
  ```rust
  let defended_distance = map.distance(order.key_point, target);
  ```
  `map_47` では、西側防衛ルートの `key_point (3,10)` と中央の敵工場 `(7,10)` の間に海（入り江）があり、実際の陸路距離は **34 マス**あるにもかかわらず、直線ヘックス距離が **4** しかない。そのため、東側攻撃ルート（`key_point (14,9)`、直線距離 7・陸路でもすぐ隣）から進軍してきた部隊にとって、敵工場 `(7,10)` が「西側防衛ルートの担当領域（`beyond_defended_route == true`）」と誤判定され、**占領目標 (`capture_target_allowed`) からも攻撃目標 (`advanced_offensive_target`) からも除外されて誰も `(7,10)` に攻め込めない**。
- **修正内容**:
  - `beyond_defended_route` において、直線ヘックス距離 `map.distance` ではなく、地上移動（例: `MovementType::Infantry` または `MovementType::Tank` の経路コスト / 到達ターン距離、あるいは既存の距離キャッシュ）を用いて防衛ルートの `key_point` と攻撃ルートの `key_point`/`staging` のどちらから実際に到達しやすいかを比較する。海や山で隔てられて防衛ルート側から直接歩いて行けない目標（陸路距離が直線距離より大幅に遠い目標）を防衛ルート側と誤認しないようにする。

#### 対象ファイル 5-B: `engine/src/ai/v4/capital_route/node_operation.rs`
- **対象箇所**: `capital_route_node_exit_satisfied`（L375〜388）
- **現状の問題**:
  `CapitalRouteNodeObjective::Capture` の終了条件が `node.state == RoadmapNodeState::Secured`（ノード内の全拠点を 100% 同時所有）のみとなっている。
  `map_20` のように、敵首都ノードの直前ノード（都市 `(16,16), (16,17), (17,16)`）が敵工場のすぐ隣にある場合、V4 が戦力比 10:1 で完全に制圧 (`ForceAdvantageState::Dominant`) していても、敵工場から湧いた歩兵 1 体が都市 1 つを踏み返すか未占領で残るだけで `RoadmapNodeState::Contesting` に留まり、**4 マス先の敵首都 `(21,16)` / `(8,3)` ノードが 60 ターン目まで永久に `Locked` のまま解放されない**。
- **修正内容**:
  - `capital_route_node_exit_satisfied` において、`CapitalRouteNodeObjective::Capture` であっても、以下の条件をすべて満たす場合は後続ノード（特に敵首都・敵本陣ノード）のロック解除を認める:
    1. `node.state == RoadmapNodeState::Secured`、**または**
    2. `node.force_advantage == ForceAdvantageState::Dominant` かつ `node.persistence_turns >= 2` であり、ノード内拠点の過半数（または `own_property_count > 0 && enemy_property_count <= 1`）を既に確保して前線が当該ノードを通過している場合。

---

## 5. 検証手順（Verification & Gates）

Codex は実装後、以下の順序でテストとベンチマーク検証を実行すること。

### Step 1: 単体テストの追加と Gate 0（コンパイル・Lint・既存テスト）
以下の回帰テストを追加した上で Gate 0 を通すこと。
1. **`formation.rs` のテスト**: `SupplyTruck` が `FreeUnits::collect` で `free.combat` に入らず、`form_attack_squads` / `absorb_surplus_combat_units` の結果として `MissionType::Attack` や `Defense` 小隊に編入されないことを検証するテスト。
2. **`production_pipeline` / `mission_input` のテスト**: 展開済み（`Reserve` または `Defense` の `firing_position` 指定）の `Artillery` が `assign_committed_combat` で同一島の `Operation.existing_units` に正しくカウントされ、既存砲台があるのに 0 門扱いされて過剰生産されないことを検証するテスト。
3. **`artillery_ferry` のテスト**: 作戦ゾーン内 (`in_zone`) に展開済みの `Artillery` が、一時的に射撃対象がないターンに同一タイルまたは前進不能な状態で即座に `Load` しないことを検証するテスト。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Step 2: Gate 1（単一シードの短縮トレース検証）
`map_20`（先攻・後攻 seed 1）と `map_47`（先攻 seed 1）を実行し、以下を確認する:
```powershell
uv run openwars-eval --mode batch --map map_20 --p1 V4 --p2 V200 --games 1 --max-turns 60 --jobs 1 --output .tmp/verify_map20_p1.md
uv run openwars-eval --mode batch --map map_20 --p1 V200 --p2 V4 --games 1 --max-turns 60 --jobs 1 --output .tmp/verify_map20_p2.md
uv run openwars-eval --mode batch --map map_47 --p1 V4 --p2 V200 --games 1 --max-turns 60 --jobs 1 --output .tmp/verify_map47_p1.md
```
**Gate 1 合格基準**:
- `SupplyTruck` をメンバーとする `MissionType::Attack` 小隊が **0 件** であること。
- `Artillery` と `SupplyTruck` の生産数が常識的な範囲（各数台〜十数台以下）に収まり、`Tank` / `MdTank` / `TankZ` 等の直接戦闘車両が主力として生産・前進していること。
- `map_20`（先攻・後攻）が 60T 時間切れではなく、敵首都占領または敵殲滅で決着すること。
- `map_47`（先攻）が T48 より大幅に早いターンで敵工場 `(7,10)`・敵首都 `(8,10)` を攻略して決着すること。

### Step 3: Gate 2（3 マップ × 先後 × 3 シードのスイープ検証）
`HANDOFF.md` のベースライン（`.tmp/bench-20261101/baseline-sweep/`）と比較するため、`map_20`, `map_40`, `map_47` の 3 シードスイープを実行する（並列数は `--jobs 8` 以下を厳守）:
```powershell
uv run openwars-eval --mode batch --map map_20 --p1 V4 --p2 V200 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map20_p1.md
uv run openwars-eval --mode batch --map map_20 --p1 V200 --p2 V4 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map20_p2.md
uv run openwars-eval --mode batch --map map_47 --p1 V4 --p2 V200 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map47_p1.md
uv run openwars-eval --mode batch --map map_47 --p1 V200 --p2 V4 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map47_p2.md
uv run openwars-eval --mode batch --map map_40 --p1 V4 --p2 V200 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map40_p1.md
uv run openwars-eval --mode batch --map map_40 --p1 V200 --p2 V4 --games 3 --max-turns 60 --jobs 6 --output .tmp/verify_sweep_map40_p2.md
```
**Gate 2 合格基準**:
- `map_40`（100% 勝率, avg ~25T）および `map_47` 後攻（100% 勝率, avg ~23T）に退行がないこと。
- `map_20` 先攻・後攻の 60T 時間切れが解消または大幅に短縮され、`map_47` 先攻の平均決着ターン（旧 53.0T）が大幅に短縮されること。
