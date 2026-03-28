# Tasks: allow-passing-allied-units

## 1. Engine実装の修正

- [x] 1.1 `engine/src/systems/movement.rs` の `calculate_reachable_tiles` を修正し、自軍ユニットを通過可能にする [x]
- [x] 1.2 `engine/src/systems/movement.rs` の `find_path_a_star` を修正し、自軍ユニットを通過可能にする [x]

## 2. テストの追加と検証

- [x] 2.1 `movement.rs` 内の単体テストに「味方ユニットの追い越し」ケースを追加する [x]
- [x] 2.2 `movement.rs` 内の単体テストに「味方ユニットのいるマスへの停止不可（輸送を除く）」ケースを追加する [x]
- [x] 2.3 既存の「敵軍ユニットによる遮断」テストが引き続きパスすることを確認する [x]
