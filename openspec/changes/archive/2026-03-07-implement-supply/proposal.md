# Change: 補給の実装（SupplyTruck・空母・拠点による燃料/弾薬補充）

## Why
ユニットは毎ターン燃料を消費し、攻撃時に弾薬を消費するが、現在は補給手段が実装されていない。
燃料切れや弾切れは死を意味するため、補給メカニズムが戦略の核となる。

## What Changes
### 補給の種類
1. **補給輸送車（SupplyTruck）による補給**  
   - 行動フェーズ中に実行。移動後も可能（`has_moved == true` でも実行可）。
   - 対象：補給輸送車の隣接マス（マンハッタン距離 = 1）にいる味方地上ユニット。
   - 単体指定補給（1 ユニットを選んで補給）と全自動補給（全隣接味方ユニットへ一括）の 2 モード。
   - 補給後に `action_completed = true` となる。資金は消費しない。

2. **空母（AircraftCarrier）による補給**  
   - 空母に搭載されている航空ユニットの燃料・弾薬を最大値まで回復する。
   - 全搭載ユニット一括の補給となる（選択補給とは別）。
   - 補給後に `action_completed = true` となる。

3. **拠点（Property）による補給**  
   - 自国の拠点（首都・都市・工場・空港・港）の上にいる味方ユニットは、ターン開始時に自動で補給を受ける。
   - 補給コストは資金から自動控除：弾薬 1 個につき 15G、燃料 1 単位につき 5G。
   - 資金不足の場合は補給されない（弾薬・燃料両方とも）。

### 補給内容
- 燃料（`fuel`）を `max_fuel` まで回復。
- 弾薬1（`ammo1`）を `max_ammo1` まで回復。
- 弾薬2（`ammo2`）を `max_ammo2` まで回復。

## Impact
- Affected specs: `unit-supply`（新規）
- Affected code: `src/domain/game_state.rs`（補給メソッド追加、`advance_turn`/`process_daily_updates`の拠点補給処理）、`src/domain/unit_roster.rs`（`can_supply: bool` フラグを `UnitStats` に追加）
