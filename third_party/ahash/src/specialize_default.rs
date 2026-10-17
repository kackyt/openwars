// OpenWarsパッチ: specialize.rs の `default fn` 既定実装（cfg(specialize) のときだけ読み込む）。
use super::CallHasher;
use crate::RandomState;
use core::hash::BuildHasher;
use core::hash::Hash;
use core::hash::Hasher;

impl<T> CallHasher for T
where
    T: Hash + ?Sized,
{
    #[inline]
    default fn get_hash<H: Hash + ?Sized>(value: &H, random_state: &RandomState) -> u64 {
        let mut hasher = random_state.build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }
}
