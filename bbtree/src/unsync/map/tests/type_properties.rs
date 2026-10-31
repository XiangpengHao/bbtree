//! Type-level properties shared with std's `BTreeMap` and its iterators:
//! auto traits, variance, size, and a `const` constructor that allocates
//! nothing.

#![forbid(unsafe_code)]

use core::cell::Cell;
use core::marker::{PhantomData, PhantomPinned};
use core::ops::RangeFull;
use core::panic::{RefUnwindSafe, UnwindSafe};
use std::collections::{BTreeMap, btree_map};
use std::rc::Rc;
use std::string::String;
use std::sync::MutexGuard;

use crate::inline_key::WithInlineKey;
use crate::unsync::map::{
    BBTreeMap, Entry, ExtractIf, IntoIter, IntoKeys, IntoValues, Iter, IterMut, Keys,
    OccupiedEntry, Range, RangeMut, VacantEntry, Values, ValuesMut,
};

/// Which auto traits a type implements.
#[derive(Debug, PartialEq, Eq)]
struct AutoTraits {
    send: bool,
    sync: bool,
    unpin: bool,
    unwind_safe: bool,
    ref_unwind_safe: bool,
}

/// Tells whether `T` implements each auto trait, where `T` is a concrete
/// type: an inherent constant below applies if its bound holds, and the
/// [`AutoTraitFallback`] default of `false` stands in otherwise.
struct AutoTraitProbe<T: ?Sized>(PhantomData<T>);

trait AutoTraitFallback {
    const SEND: bool = false;
    const SYNC: bool = false;
    const UNPIN: bool = false;
    const UNWIND_SAFE: bool = false;
    const REF_UNWIND_SAFE: bool = false;
}

impl<T: ?Sized> AutoTraitFallback for AutoTraitProbe<T> {}

impl<T: ?Sized + Send> AutoTraitProbe<T> {
    const SEND: bool = true;
}

impl<T: ?Sized + Sync> AutoTraitProbe<T> {
    const SYNC: bool = true;
}

impl<T: ?Sized + Unpin> AutoTraitProbe<T> {
    const UNPIN: bool = true;
}

impl<T: ?Sized + UnwindSafe> AutoTraitProbe<T> {
    const UNWIND_SAFE: bool = true;
}

impl<T: ?Sized + RefUnwindSafe> AutoTraitProbe<T> {
    const REF_UNWIND_SAFE: bool = true;
}

/// The [`AutoTraits`] of a concrete type.
macro_rules! auto_traits {
    ($type:ty) => {
        AutoTraits {
            send: AutoTraitProbe::<$type>::SEND,
            sync: AutoTraitProbe::<$type>::SYNC,
            unpin: AutoTraitProbe::<$type>::UNPIN,
            unwind_safe: AutoTraitProbe::<$type>::UNWIND_SAFE,
            ref_unwind_safe: AutoTraitProbe::<$type>::REF_UNWIND_SAFE,
        }
    };
}

/// Checks that our map from `$key` to `$value`, and each iterator over it,
/// has the auto traits of std's, for each pair.
macro_rules! assert_auto_traits_match_std {
    ($($key:ty => $value:ty),+ $(,)?) => {$(
        assert_eq!(
            auto_traits!(BBTreeMap<$key, $value>),
            auto_traits!(BTreeMap<$key, $value>),
            "a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(Iter<'static, $key, $value>),
            auto_traits!(btree_map::Iter<'static, $key, $value>),
            "Iter over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(Keys<'static, $key, $value>),
            auto_traits!(btree_map::Keys<'static, $key, $value>),
            "Keys over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(Values<'static, $key, $value>),
            auto_traits!(btree_map::Values<'static, $key, $value>),
            "Values over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(Range<'static, $key, $value>),
            auto_traits!(btree_map::Range<'static, $key, $value>),
            "Range over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(IterMut<'static, $key, $value>),
            auto_traits!(btree_map::IterMut<'static, $key, $value>),
            "IterMut over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(ValuesMut<'static, $key, $value>),
            auto_traits!(btree_map::ValuesMut<'static, $key, $value>),
            "ValuesMut over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(Entry<'static, $key, $value>),
            auto_traits!(btree_map::Entry<'static, $key, $value>),
            "Entry in a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(OccupiedEntry<'static, $key, $value>),
            auto_traits!(btree_map::OccupiedEntry<'static, $key, $value>),
            "OccupiedEntry in a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(VacantEntry<'static, $key, $value>),
            auto_traits!(btree_map::VacantEntry<'static, $key, $value>),
            "VacantEntry in a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(ExtractIf<'static, $key, $value, RangeFull, fn(&$key, &mut $value) -> bool>),
            auto_traits!(btree_map::ExtractIf<'static, $key, $value, RangeFull, fn(&$key, &mut $value) -> bool>),
            "ExtractIf on a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(IntoIter<$key, $value>),
            auto_traits!(btree_map::IntoIter<$key, $value>),
            "IntoIter over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(IntoKeys<$key, $value>),
            auto_traits!(btree_map::IntoKeys<$key, $value>),
            "IntoKeys over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(IntoValues<$key, $value>),
            auto_traits!(btree_map::IntoValues<$key, $value>),
            "IntoValues over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
        assert_eq!(
            auto_traits!(RangeMut<'static, $key, $value>),
            auto_traits!(btree_map::RangeMut<'static, $key, $value>),
            "RangeMut over a map from {} to {}",
            stringify!($key),
            stringify!($value),
        );
    )+};
}

#[test]
fn auto_traits_match_std() {
    // The probe itself.
    let all = AutoTraits {
        send: true,
        sync: true,
        unpin: true,
        unwind_safe: true,
        ref_unwind_safe: true,
    };
    assert_eq!(auto_traits!(u64), all);
    let rc = auto_traits!(Rc<u8>);
    assert!(!rc.send && !rc.sync);
    assert!(!auto_traits!(PhantomPinned).unpin);
    assert!(!auto_traits!(&mut u8).unwind_safe);
    assert!(!auto_traits!(Cell<u8>).ref_unwind_safe);

    assert_eq!(auto_traits!(BBTreeMap<u64, u64>), all);
    assert_auto_traits_match_std!(
        u64 => u64,
        String => String,
        // Neither `Send` nor `Sync`.
        Rc<u8> => u8,
        u8 => Rc<u8>,
        // `Send` but not `Sync`, and not `RefUnwindSafe`.
        Cell<u8> => u8,
        u8 => Cell<u8>,
        // `Sync` but not `Send`.
        MutexGuard<'static, u8> => u8,
        u8 => MutexGuard<'static, u8>,
        // Not `Unpin`.
        PhantomPinned => u8,
        u8 => PhantomPinned,
        // `RefUnwindSafe` but not `UnwindSafe`.
        &'static mut u8 => u8,
        u8 => &'static mut u8,
    );
    // The strategy is only a marker: it changes no auto trait.
    assert_eq!(
        auto_traits!(BBTreeMap<String, Rc<u8>, WithInlineKey>),
        auto_traits!(BTreeMap<String, Rc<u8>>)
    );
    assert_eq!(
        auto_traits!(BBTreeMap<String, Cell<u8>, WithInlineKey>),
        auto_traits!(BTreeMap<String, Cell<u8>>)
    );
}

/// Compiles only if the map is covariant in its keys and values, as std's
/// is.
fn shorten_lifetimes<'a>(
    map: BBTreeMap<&'static str, &'static str>,
) -> BBTreeMap<&'a str, &'a str> {
    map
}

#[test]
fn covariant_in_keys_and_values() {
    let local = String::from("local");
    let mut map = BBTreeMap::new();
    map.insert("static", "value");
    let mut map = shorten_lifetimes(map);
    map.insert(local.as_str(), local.as_str());
    assert_eq!(map.get("static"), Some(&"value"));
    assert_eq!(map.get("local"), Some(&"local"));
}

/// Our iterators over a map whose keys and values live for `'k`.
type StrIterators<'a, 'k> = (
    Iter<'a, &'k str, &'k str>,
    Keys<'a, &'k str, &'k str>,
    Values<'a, &'k str, &'k str>,
    Range<'a, &'k str, &'k str>,
);

/// Compiles only if the iterators are covariant in their keys and values,
/// as std's are.
fn shorten_iterator_lifetimes<'a>(iterators: StrIterators<'a, 'static>) -> StrIterators<'a, 'a> {
    iterators
}

#[test]
fn iterators_are_covariant_in_keys_and_values() {
    let mut map = BBTreeMap::new();
    map.insert("static", "value");
    let iterators = (
        map.iter(),
        map.keys(),
        map.values(),
        map.range::<&str, _>(..),
    );
    let (iter, keys, values, range) = shorten_iterator_lifetimes(iterators);
    assert_eq!(
        iter.count() + keys.count() + values.count() + range.count(),
        4
    );
}

/// Each end of a walk holds a node stack (`tree/node_stack.rs`): a node
/// pointer and a `u16` index for each of 24 levels, the most a tree can have
/// (D22), and a one-byte count of the levels in use, 248 bytes. Every walk
/// adds the root, as a tagged pointer; ranges add a leaf run, and the
/// whole-map iterators, which walk as ranges, a count besides. std's, whose
/// nodes point to their parents, need 72 bytes.
#[test]
#[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
fn iterator_sizes() {
    assert_eq!(size_of::<Iter<'_, u64, u64>>(), 552);
    assert_eq!(size_of::<Keys<'_, u64, u64>>(), 552);
    assert_eq!(size_of::<Range<'_, u64, u64>>(), 544);
    assert_eq!(size_of::<IterMut<'_, u64, u64>>(), 552);
    assert_eq!(size_of::<RangeMut<'_, u64, u64>>(), 544);
    assert_eq!(size_of::<btree_map::Iter<'_, u64, u64>>(), 72);
}

#[test]
#[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
fn one_cache_line() {
    // Three words, as std's map, and what every key shares (D50).
    assert_eq!(
        size_of::<BBTreeMap<u64, u64>>(),
        size_of::<BTreeMap<u64, u64>>() + 40
    );
    assert_eq!(size_of::<BBTreeMap<u64, u64>>(), 64);
    assert_eq!(size_of::<BBTreeMap<String, u64, WithInlineKey>>(), 64);
}

#[test]
fn new_is_const_and_allocates_nothing() {
    const EMPTY: BBTreeMap<String, String> = BBTreeMap::new();
    let map = EMPTY;
    assert!(!map.tree.has_root());
    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
    assert_eq!(map.get("absent"), None);
    assert!(!map.contains_key("absent"));
    map.check_invariants();

    let mut map: BBTreeMap<u64, u64> = BBTreeMap::default();
    assert!(!map.tree.has_root());
    assert_eq!(map.get_mut(&1), None);
}
