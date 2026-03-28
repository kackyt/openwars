# Design: allow-passing-allied-units

## Context

現在の実装では、`calculate_reachable_tiles` および `find_path_a_star` において、`unit_positions.contains_key(&position)` のチェックにより、いかなるユニットが存在するマスも「通過不可」として扱われています。
これを、自軍ユニットであれば通過可能（探索の継続を許可）とするように変更します。

## Goals

- 経路探索において、自軍ユニットが占有するマスを通過コストを支払って通り抜けることができるようにする。
- 敵軍ユニットが占有するマス、および敵軍のZOC（ゾーン・オブ・コントロール）による停止規則は維持する。
- 重なり（同じマスに複数のユニットが留まること）は、輸送ユニットへの積載を除き、引き続き禁止する。

## Proposed Changes

### `engine/src/systems/movement.rs`

#### `calculate_reachable_tiles` の修正

1. 探索ループ内での拡張停止条件の緩和
   - `if position != start && unit_positions.contains_key(&position) { continue; }` を削除または条件変更。
   - 自軍ユニットの場合は `continue` せずに隣接マスの探索を続行するようにします。
2. 隣接マスのフィルタリングの修正
   - 隣接マスにユニットがいる場合のチェックで、自軍ユニットであれば `continue` せずに探索候補に入れるようにします。

#### `find_path_a_star` の修正

- `calculate_reachable_tiles` と同様に、拡張停止条件と隣接マスのフィルタリングを修正します。

#### 移動終点のチェック（維持）

- `calculate_reachable_tiles` の末尾にある `reachable.retain` によるフィルタリングは現状のまま維持します。これにより、「通過はできるが、そのマスで止まることはできない（空いているか積載可能な輸送ユニットである必要がある）」というルールが担保されます。

## Alternative Considerations

### ZOCの扱い
- 自軍ユニットがいるマスであっても、敵軍のZOC（隣接マスに敵がいる状態）に該当する場合は、進入した時点で強制的に移動を終了します。
- これにより、「味方ユニットが敵を抑えている間はスルーできる」といった例外を認めず、既存の戦闘ルールとの整合性を保ちます。

## Risks / Trade-offs

- 経路探索の計算量: 通過可能なマスが増えるため、探索空間がわずかに広がりますが、通常のマップサイズでは無視できるレベルです。
- AIへの影響: AIもこの新ルールを利用して移動できるようになります。
