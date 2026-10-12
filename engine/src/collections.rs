//! 決定的なハッシュ表。
//!
//! std の `HashMap` / `HashSet` は既定で `RandomState` を使い、プロセス・スレッドごとに
//! 乱数の鍵で反復順が変わる。AIの判断が反復順に依存する箇所が残っていると、同じseedの
//! 対局でもプロセスや並列ワーカーの割り当てによって行動列が分岐する。
//!
//! engine では鍵を固定した SipHash（`DefaultHasher` の既定値）を使う別名を正とし、
//! 反復順を入力の挿入履歴だけで決まるようにする。生成は `HashMap::default()`、
//! リテラルからは `HashMap::from_iter([...])` を使う（`new()` / `from()` は
//! `RandomState` 専用のため使えない）。

use std::collections::hash_map::DefaultHasher;
use std::hash::BuildHasherDefault;

/// 鍵を固定したハッシュ関数の生成器。
pub type DeterministicState = BuildHasherDefault<DefaultHasher>;

/// 反復順がプロセス・スレッドに依存しないHashMap。
pub type HashMap<K, V> = std::collections::HashMap<K, V, DeterministicState>;

/// 反復順がプロセス・スレッドに依存しないHashSet。
pub type HashSet<T> = std::collections::HashSet<T, DeterministicState>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_order_does_not_depend_on_instance() {
        // RandomStateなら生成ごとに鍵が変わり、同じ挿入列でも反復順が変わり得る。
        let build = || (0..64_u32).map(|v| v * 7919).collect::<HashSet<_>>();
        let first = build().into_iter().collect::<Vec<_>>();
        for _ in 0..8 {
            assert_eq!(build().into_iter().collect::<Vec<_>>(), first);
        }
        let worker = std::thread::spawn(move || build().into_iter().collect::<Vec<_>>());
        assert_eq!(worker.join().unwrap(), first);
    }
}
