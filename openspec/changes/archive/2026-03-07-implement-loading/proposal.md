# Change: 搭載の実装（輸送ユニットへのユニット搭載・下車）

## Why
輸送ヘリ・輸送船・装甲車・空母などの輸送ユニットは戦略的な移動手段だが、搭載ロジックがまだ実装されていない。搭載機能は地上ユニットの海越え・高機動展開・空母による航空運用の基盤となる。

## What Changes

### データモデル
- `UnitStats` に `max_cargo: u32`（最大搭載数, 0 = 搭載不可）と `loadable_types: Vec<UnitType>`（搭載可能ユニット種別一覧）を追加する。
- `Unit` に `cargo: Vec<usize>`（搭載ユニットのインデックス一覧）および `transport_index: Option<usize>`（自分を運ぶ輸送ユニットのインデックス）を追加する。

### 搭載ルール
| 輸送ユニット | 搭載可能ユニット | 最大搭載数 |
|------------|--------------|----------|
| 装甲車 | 歩兵, 戦闘工兵 | 1 |
| 補給輸送車 | 砲台 | 1 |
| 輸送ヘリ | 歩兵, 戦闘工兵 | 2 |
| 輸送船 | 地上部隊（全地上ユニット） | 2 |
| 空母 | 航空部隊（全航空ユニット） | 2 |

### 操作ルール
1. **自動搭載**: 輸送ユニットと搭載可能ユニットが同じマスに移動したとき、容量内であれば自動的に搭載される。`move_unit` の終了時に位置を照合して搭載を行う。
2. **下車**: `unload_unit(transport_index, cargo_index_in_cargo, target_x, target_y)` で実施。次のターン以降のみ可能（搭載ターンに `cargo_unit.action_completed = true` になるため）。下車先は輸送ユニットの隣接マスでなければならない。下車したユニットは `action_completed = true`（そのターンは攻撃・占領不可）。
3. **搭載中の行動制限**: 搭載中のユニットは移動・攻撃・占領など一切の行動ができない。
4. **ダメージ連動**: 輸送ユニットが攻撃を受けた際、搭載ユニットは輸送ユニットと同じ HP 値まで引き下げられる（輸送ユニットが破壊されたら搭載ユニットも消滅）。

## Impact
- Affected specs: `unit-loading`（新規）
- Affected code:
  - `src/domain/unit_roster.rs`（`UnitStats` にフィールド追加・`Unit` に `cargo` / `transport_index` を追加）
  - `src/domain/game_state.rs`（`move_unit` への自動搭載ロジック・`unload_unit` 追加・攻撃時ダメージ連動処理追加）
