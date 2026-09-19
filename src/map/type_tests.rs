//! Type-level properties shared with std's `BTreeMap`: auto traits,
//! variance, size, and a `const` constructor that allocates nothing.

#![forbid(unsafe_code)]

use core::cell::Cell;
use core::marker::{PhantomData, PhantomPinned};
use core::panic::{RefUnwindSafe, UnwindSafe};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::string::String;
use std::sync::MutexGuard;

use super::BBTreeMap;

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

/// Checks that our map from `$key` to `$value` has the auto traits of
/// std's, for each pair.
macro_rules! assert_auto_traits_match_std {
    ($($key:ty => $value:ty),+ $(,)?) => {$(
        assert_eq!(
            auto_traits!(BBTreeMap<$key, $value>),
            auto_traits!(BTreeMap<$key, $value>),
            "a map from {} to {}",
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

#[test]
fn three_words_like_std() {
    assert_eq!(size_of::<BBTreeMap<u64, u64>>(), 24);
    assert_eq!(
        size_of::<BBTreeMap<u64, u64>>(),
        size_of::<BTreeMap<u64, u64>>()
    );
}

#[test]
fn new_is_const_and_allocates_nothing() {
    const EMPTY: BBTreeMap<String, String> = BBTreeMap::new();
    let map = EMPTY;
    assert!(map.root.is_none());
    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
    assert_eq!(map.get("absent"), None);
    assert!(!map.contains_key("absent"));
    map.check_invariants();

    let mut map: BBTreeMap<u64, u64> = BBTreeMap::default();
    assert!(map.root.is_none());
    assert_eq!(map.get_mut(&1), None);
}
