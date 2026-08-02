## 1. Engine

- [x] 1.1 空母搭載ユニットの部分HP修理・無償リソース補給を実装する
- [x] 1.2 搭載航空ユニットを日次燃料処理から除外する
- [x] 1.3 輸送ユニットHP同期を被弾イベント駆動へ変更する

## 2. Tests

- [x] 2.1 空母の通常修理、部分修理、無償補給、搭載航空機の墜落防止をテストする
- [x] 2.2 被弾時同期と平時にHPを巻き戻さないことをテストする

## 3. Verification

- [x] 3.1 `pnpm exec openspec validate --all` を実行する
- [x] 3.2 `cargo test`、`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings` を実行する
