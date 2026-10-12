# ahash 0.8.12（OpenWars決定性パッチ）

crates.io の `ahash 0.8.12` に、乱数の鍵を使わない変更だけを加えたもの。
ワークスペースの `Cargo.toml` の `[patch.crates-io]` から参照する。

## 理由

`bevy_utils` の `std` 機能が `ahash/runtime-rng` を有効にするため、`AHasher::default()`
の「固定鍵」も `getrandom` でプロセスごとに決まる。`bevy_ecs 0.15` の
`QueryState::update_archetypes` はコンポーネント索引（ahashのHashMap）のキー順に
アーキタイプを登録するので、同じ盤面でもクエリのEntity列挙順がプロセスごとに変わり、
同一seedのAI対局が再現しなかった。

## 変更点（`src/random_state.rs` のみ）

- `get_fixed_seeds` を常に定数 `[PI, PI2]` にする（`runtime-rng` / `compile-time-rng` でも乱数を使わない）。
- `DefaultRandomSource` の初期値と増分にアドレス（ASLR）を使わない。

ライセンスは元のとおり MIT OR Apache-2.0（同梱の LICENSE-* を参照）。
- third_party/ahash/src/lib.rs: 先頭に `#![allow(warnings)]`（ワークスペースの `-D warnings` を外部コードへ適用しないため）。
