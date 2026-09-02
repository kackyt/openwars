//! AI候補を入力順のまま評価する、native/WASM共通の実行基盤。
//!
//! nativeではRayonで独立候補を並列評価する。WASMではthreadを要求せず同じAPIを
//! 直列実行する。どちらも結果順を入力順に固定し、同点時のAI判断を変えない。

/// 入力順を維持したまま、互いに独立な要素を評価する。
pub(crate) fn map_ordered<T, R, F>(items: Vec<T>, evaluate: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;

        // 小さい列はthread poolへ渡す固定費の方が大きい。島が1つだけのmapや
        // beam初段では直列のままにし、十分な独立候補がある場合だけ並列化する。
        if items.len() < 4 {
            return items.into_iter().map(evaluate).collect();
        }
        items.into_par_iter().map(evaluate).collect()
    }

    #[cfg(target_arch = "wasm32")]
    {
        items.into_iter().map(evaluate).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_map_preserves_input_order() {
        let actual = map_ordered(vec![3_u32, 1, 4, 2], |value| value * value);

        assert_eq!(actual, vec![9, 1, 16, 4]);
    }
}
