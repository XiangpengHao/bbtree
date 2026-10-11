//! Type-level properties shared with std's `BTreeSet` and its iterators:
//! variance, size, and a `const` constructor. The auto traits are the map's
//! (`map/type_tests.rs`), whose values here are a unit struct.

#![forbid(unsafe_code)]

use std::string::String;

use super::{BBTreeSet, Iter, Range};
use crate::inline_key::WithInlineKey;
use crate::map::BBTreeMap;

/// Compiles only if the set is covariant in its elements, as std's is.
fn shorten_lifetimes<'a>(set: BBTreeSet<&'static str>) -> BBTreeSet<&'a str> {
    set
}

#[test]
fn covariant_in_elements() {
    let local = String::from("local");
    let mut set = BBTreeSet::new();
    set.insert("static");
    let mut set = shorten_lifetimes(set);
    set.insert(local.as_str());
    assert!(set.contains("static"));
    assert!(set.contains("local"));
}

/// Our iterators over a set whose elements live for `'k`.
type StrIterators<'a, 'k> = (Iter<'a, &'k str>, Range<'a, &'k str>);

/// Compiles only if the iterators are covariant in their elements, as std's
/// are.
fn shorten_iterator_lifetimes<'a>(iterators: StrIterators<'a, 'static>) -> StrIterators<'a, 'a> {
    iterators
}

#[test]
fn iterators_are_covariant_in_elements() {
    let set = BBTreeSet::from(["static"]);
    let iterators = (set.iter(), set.range::<&str, _>(..));
    let (iter, range) = shorten_iterator_lifetimes(iterators);
    assert_eq!(iter.count() + range.count(), 2);
}

/// The map's four words, its values taking no space.
#[test]
fn four_words_like_the_map() {
    assert_eq!(size_of::<BBTreeSet<u64>>(), 32);
    assert_eq!(size_of::<BBTreeSet<u64>>(), size_of::<BBTreeMap<u64, ()>>());
    assert_eq!(size_of::<BBTreeSet<String, WithInlineKey>>(), 32);
}

#[test]
fn new_is_const() {
    const EMPTY: BBTreeSet<String> = BBTreeSet::new();
    let set = EMPTY;
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
    assert!(!set.contains("absent"));
    assert_eq!(set.first(), None);
}
