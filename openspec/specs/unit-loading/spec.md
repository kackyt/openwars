# unit-loading Specification

## Purpose
TBD - created by archiving change implement-loading. Update Purpose after archive.
## Requirements
### Requirement: 輸送ユニットへの自動搭載
When a unit moves onto the same tile as a friendly transport unit with available capacity and matching loadable type, it SHALL be automatically loaded into the transport. Loaded units MUST have `action_completed` set to true and MUST NOT perform any actions while loaded.
対象ユニットが同じマスにいる味方輸送ユニットへ移動した際、容量と種別の条件を満たす場合に自動搭載されなければならない（MUST）。搭載されたユニットは `action_completed = true` となり、搭載中は一切の行動を行えない。

#### Scenario: 条件を満たす自動搭載
- **GIVEN** 輸送ユニットが搭載対象ユニットを `loadable_types` に含み、`cargo.len() < max_cargo` である
- **WHEN** 搭載対象ユニットが同じマスへ移動する
- **THEN** 搭載対象ユニットは輸送ユニットの `cargo` に追加され、`transport_index` が輸送ユニットを指し、`action_completed = true` となる

#### Scenario: 搭載対象外ユニットの搭載拒否
- **WHEN** 輸送ユニットの `loadable_types` に含まれないユニット種別が同じマスへ移動する
- **THEN** 搭載は発生せず、ユニットは通常どおりそのマスに留まる

#### Scenario: 容量超過による搭載拒否
- **WHEN** 輸送ユニットの `cargo.len() >= max_cargo` の状態でユニットが同じマスへ移動する
- **THEN** 搭載は発生しない

### Requirement: 下車アクション
A transport unit SHALL allow a loaded unit to disembark to an adjacent reachable tile. The disembarked unit MUST have `action_completed` set to true and MUST NOT attack or capture in the same turn.
輸送ユニットは搭載ユニットを隣接の移動可能マスへ下車させなければならない（MUST）。下車したユニットは `action_completed = true` となり、同ターン中は攻撃・占領ができない。

#### Scenario: 正常な下車
- **GIVEN** 搭載ユニットが輸送ユニット内にいる（`action_completed == true` で次ターン以降）
- **WHEN** `unload_unit` で隣接の移動可能マスを指定する
- **THEN** 搭載ユニットが指定マスに配置され、輸送ユニットの `cargo` から除去される。下車ユニットの `action_completed = true` となる

#### Scenario: 非隣接マスへの下車失敗
- **WHEN** `unload_unit` で輸送ユニットから距離 > 1 のマスを指定する
- **THEN** 下車は無効（エラー）となる

#### Scenario: 搭載ターン内の下車禁止
- **WHEN** 搭載されたターンと同じターンに `unload_unit` を呼ぶ（`action_completed == true` かつ `has_moved == true` で搭載直後）
- **THEN** 下車は無効（エラー）となる

### Requirement: 輸送ユニット被弾時の搭載ユニット HP 同期
When a transport unit takes damage, all loaded units SHALL have their HP reduced to match the transport's HP. If the transport is destroyed, all loaded units MUST also be destroyed.
輸送ユニットがダメージを受けると、搭載中のすべてのユニットの HP が輸送ユニットと同じ値に引き下げられなければならない（MUST）。輸送ユニットが撃破された場合、搭載ユニットも同時に消滅する。

#### Scenario: 輸送ユニット被弾による HP 同期
- **WHEN** 輸送ユニットが攻撃を受けて HP が減少する
- **THEN** 搭載ユニット全員の HP が輸送ユニットの HP と同じ値に引き下げられる

#### Scenario: 輸送ユニット撃破による搭載ユニット消滅
- **WHEN** 輸送ユニットが撃破（hp == 0）される
- **THEN** 搭載中のすべてのユニットも hp = 0 となり消滅し、勝利条件の再チェックが行われる

