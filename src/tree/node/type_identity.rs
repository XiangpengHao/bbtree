//! Lifetime-erased type identity, and the views of a value that it proves
//! sound. [`Auto`] uses them to recognize std byte strings and read their
//! bytes, and to recognize std integers and use their [`InlineKey`] impls
//! (D18, D31). A key with an `InlineKey` impl, under either strategy, uses
//! one to treat a lookup key as a key.
//!
//! [`Auto`]: crate::Auto
//! [`InlineKey`]: crate::InlineKey

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::TypeId;
use core::borrow::Borrow;
use core::marker::PhantomData;
use core::{mem, ptr, slice};

use crate::inline_key::{InlineKey, InlineWidth, InlineWord};

/// A type's identity with its lifetimes erased: `&'a str` and
/// `&'static str` have the same one. `TypeId::of` needs `'static` types,
/// and keys need not be.
///
/// dtolnay's `typeid` trick, vendored (D2; MIT OR Apache-2.0). Comparing
/// two of them folds to a constant after monomorphization.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ErasedTypeId(TypeId);

impl ErasedTypeId {
    #[inline(always)]
    fn of<T: ?Sized>() -> Self {
        /// `Any`, with the `'static` bound on the method instead of the
        /// trait, so that `PhantomData<T>` implements it for any `T`.
        trait NonStaticAny {
            fn type_id(&self) -> TypeId
            where
                Self: 'static;
        }

        impl<T: ?Sized> NonStaticAny for PhantomData<T> {
            #[inline(always)]
            fn type_id(&self) -> TypeId
            where
                Self: 'static,
            {
                TypeId::of::<T>()
            }
        }

        let phantom = PhantomData::<T>;
        // SAFETY: the two references differ only in the trait object's
        // lifetime bound, not in layout or vtable. The `'static` bound
        // they claim only lets `type_id` call `TypeId::of::<T>()`, which
        // reads nothing and returns the identity of `T` as compiled,
        // where lifetimes no longer exist. Nothing borrowed is used
        // beyond this call.
        let object =
            unsafe { mem::transmute::<&dyn NonStaticAny, &(dyn NonStaticAny + 'static)>(&phantom) };
        Self(object.type_id())
    }

    /// Whether `T` is `U`, lifetimes aside.
    #[inline(always)]
    fn is<T: ?Sized, U: ?Sized>() -> bool {
        Self::of::<T>() == Self::of::<U>()
    }
}

/// Proof that `T` is one of the std byte-string types that [`Auto`]
/// gives inline keys, lifetimes aside, and how it holds its bytes. Every
/// one of them orders like its bytes.
///
/// [`Auto`]: crate::Auto
pub(in crate::tree) struct ByteString<T: ?Sized> {
    representation: ByteRepresentation,
    _type: PhantomData<fn(&T)>,
}

impl<T: ?Sized> Clone for ByteString<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for ByteString<T> {}

/// How a recognized byte string holds its bytes.
#[derive(Clone, Copy)]
enum ByteRepresentation {
    /// The value is its bytes: `str` and `[u8]`.
    Slice,
    /// The value is its bytes, as many as every value of the type has:
    /// `[u8; 16]`, `[u8; 20]` and `[u8; 32]`.
    Array,
    String,
    ByteVec,
    BoxedStr,
    BoxedBytes,
    StrRef,
    BytesRef,
}

impl<T: ?Sized> ByteString<T> {
    /// The proof, if `T` is `String`, `str`, `Vec<u8>`, `[u8]`,
    /// `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`, `[u8; 20]`
    /// or `[u8; 32]`, lifetimes aside. Folds to a constant after
    /// monomorphization.
    ///
    /// Unoptimized builds compare type identities at run time, on every
    /// call. So the candidates are first narrowed by what needs no
    /// comparison: whether `T` is unsized, and whether it owns memory to
    /// drop.
    #[inline(always)]
    pub(in crate::tree) fn recognize() -> Option<Self> {
        let is_unsized = mem::size_of::<&T>() > mem::size_of::<&u8>();
        let representation = if is_unsized {
            if ErasedTypeId::is::<T, str>() || ErasedTypeId::is::<T, [u8]>() {
                ByteRepresentation::Slice
            } else {
                return None;
            }
        } else if mem::needs_drop::<T>() {
            if ErasedTypeId::is::<T, String>() {
                ByteRepresentation::String
            } else if ErasedTypeId::is::<T, Vec<u8>>() {
                ByteRepresentation::ByteVec
            } else if ErasedTypeId::is::<T, Box<str>>() {
                ByteRepresentation::BoxedStr
            } else if ErasedTypeId::is::<T, Box<[u8]>>() {
                ByteRepresentation::BoxedBytes
            } else {
                return None;
            }
        } else if ErasedTypeId::is::<T, [u8; 16]>()
            || ErasedTypeId::is::<T, [u8; 20]>()
            || ErasedTypeId::is::<T, [u8; 32]>()
        {
            ByteRepresentation::Array
        } else if ErasedTypeId::is::<T, &str>() {
            ByteRepresentation::StrRef
        } else if ErasedTypeId::is::<T, &[u8]>() {
            ByteRepresentation::BytesRef
        } else {
            return None;
        };
        Some(Self {
            representation,
            _type: PhantomData,
        })
    }

    /// Whether a value is its bytes, as an array is, rather than a
    /// handle to bytes elsewhere: two keys' bytes then compare without
    /// reading past the node that holds them.
    #[inline(always)]
    pub(in crate::tree) fn holds_its_bytes(self) -> bool {
        match self.representation {
            ByteRepresentation::Slice | ByteRepresentation::Array => true,
            ByteRepresentation::String
            | ByteRepresentation::ByteVec
            | ByteRepresentation::BoxedStr
            | ByteRepresentation::BoxedBytes
            | ByteRepresentation::StrRef
            | ByteRepresentation::BytesRef => false,
        }
    }

    /// The bytes of `value`. Their length comes from `value` itself: for
    /// the types that hold their bytes elsewhere, from its length field,
    /// without reading what it points to.
    #[inline(always)]
    pub(in crate::tree) fn bytes<'v>(self, value: &'v T) -> &'v [u8] {
        let size = mem::size_of_val(value);
        let value = ptr::from_ref(value);
        // SAFETY: `recognize` found `T` to be the type `representation`
        // names, lifetimes aside, so `value` points to a valid one, and
        // each arm reads it as that type for `'v`, the borrow it came
        // with. In place, its `size` bytes are initialized `u8`s. A
        // borrowed string `&'b str` or `&'b [u8]` is read as `&'v str`
        // or `&'v [u8]`, which it outlives: `&'v &'b str` is well-formed
        // only if `'b: 'v`.
        unsafe {
            match self.representation {
                ByteRepresentation::Slice | ByteRepresentation::Array => {
                    slice::from_raw_parts(value.cast::<u8>(), size)
                }
                ByteRepresentation::String => (&*value.cast::<String>()).as_bytes(),
                ByteRepresentation::ByteVec => (&*value.cast::<Vec<u8>>()).as_slice(),
                ByteRepresentation::BoxedStr => (&*value.cast::<Box<str>>()).as_bytes(),
                ByteRepresentation::BoxedBytes => &*value.cast::<Box<[u8]>>(),
                ByteRepresentation::StrRef => (*value.cast::<&'v str>()).as_bytes(),
                ByteRepresentation::BytesRef => *value.cast::<&'v [u8]>(),
            }
        }
    }
}

/// Proof that `T` is one of the std integer types that [`Auto`] gives
/// inline keys, `u64`, `i64`, `u128` or `i128`, with that type's
/// [`InlineKey`] impl as one for `T`.
///
/// [`Auto`]: crate::Auto
pub(in crate::tree) struct StdInteger<T> {
    inline_key_at: fn(&T, usize) -> InlineWord,
    width: InlineWidth,
}

impl<T> Clone for StdInteger<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for StdInteger<T> {}

impl<T> StdInteger<T> {
    /// The proof, if `T` is `u64`, `i64`, `u128` or `i128`. Folds to a
    /// constant after monomorphization.
    ///
    /// Unoptimized builds compare type identities at run time, so the
    /// candidates are first narrowed by `T`'s size.
    #[inline(always)]
    pub(in crate::tree) fn recognize() -> Option<Self> {
        // SAFETY: each arm passes the type that `T` was just found to be;
        // integers have no lifetimes to erase.
        unsafe {
            match mem::size_of::<T>() {
                8 if ErasedTypeId::is::<T, u64>() => Some(Self::of::<u64>()),
                8 if ErasedTypeId::is::<T, i64>() => Some(Self::of::<i64>()),
                16 if ErasedTypeId::is::<T, u128>() => Some(Self::of::<u128>()),
                16 if ErasedTypeId::is::<T, i128>() => Some(Self::of::<i128>()),
                _ => None,
            }
        }
    }

    /// `Integer`'s impl, as one for `T`.
    ///
    /// # Safety
    ///
    /// `T` is `Integer`.
    #[inline(always)]
    unsafe fn of<Integer: InlineKey>() -> Self {
        let inline_key_at: fn(&Integer, usize) -> InlineWord = Integer::inline_key_at;
        // SAFETY: `T` is `Integer` (the caller's promise), so the two
        // function pointer types are one type.
        let inline_key_at = unsafe {
            mem::transmute::<fn(&Integer, usize) -> InlineWord, fn(&T, usize) -> InlineWord>(
                inline_key_at,
            )
        };
        Self {
            inline_key_at,
            width: Integer::WIDTH,
        }
    }

    /// The integer's [`InlineKey::inline_key_at`].
    #[inline(always)]
    pub(in crate::tree) fn inline_key_at(self) -> fn(&T, usize) -> InlineWord {
        self.inline_key_at
    }

    /// The integer's [`InlineKey::WIDTH`].
    #[inline(always)]
    pub(in crate::tree) fn width(self) -> InlineWidth {
        self.width
    }
}

/// Proof that a lookup key type `Q` is the key type `K` itself,
/// lifetimes included.
pub(in crate::tree) struct SameType<K, Q: ?Sized>(PhantomData<fn(&Q) -> &K>);

impl<K: Borrow<Q>, Q: ?Sized> SameType<K, Q> {
    /// The proof, if `Q` is `K` lifetimes aside. Then `K: Borrow<Q>` makes
    /// them the same type, lifetimes included. The only impl that can prove
    /// `Borrow<Q>` for a type that is `Q` lifetimes aside is std's
    /// `impl<T: ?Sized> Borrow<T> for T`: any other would overlap with it
    /// (coherence ignores lifetimes) and is rejected. That impl makes them
    /// one type. Folds to a constant after monomorphization.
    #[inline(always)]
    pub(in crate::tree) fn recognize() -> Option<Self> {
        ErasedTypeId::is::<K, Q>().then_some(Self(PhantomData))
    }

    /// `key` as the key type it is.
    #[inline(always)]
    pub(in crate::tree) fn cast(self, key: &Q) -> &K {
        // SAFETY: `Q` is `K`, lifetimes included (`recognize`), so `key`
        // points to a valid `K`, borrowed for as long as it says.
        unsafe { &*ptr::from_ref(key).cast::<K>() }
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    use super::{ByteString, SameType};
    use crate::inline_key::InlineWord;

    /// The std byte strings `Auto` recognizes, read in place or through
    /// their pointer, lifetimes aside; and nothing else.
    #[test]
    fn byte_strings_are_recognized_by_type() {
        fn bytes_of<T: ?Sized>(value: &T) -> Option<&[u8]> {
            ByteString::<T>::recognize().map(|byte_string| byte_string.bytes(value))
        }
        let text = "bytes".to_owned();
        let expected = Some(text.as_bytes());
        assert_eq!(bytes_of::<String>(&text), expected);
        assert_eq!(bytes_of::<str>(&text), expected);
        assert_eq!(bytes_of::<Vec<u8>>(&text.clone().into_bytes()), expected);
        assert_eq!(bytes_of::<[u8]>(text.as_bytes()), expected);
        assert_eq!(bytes_of::<Box<str>>(&Box::from(text.as_str())), expected);
        assert_eq!(bytes_of::<Box<[u8]>>(&Box::from(text.as_bytes())), expected);
        // A borrowed string whose lifetime is not `'static`.
        assert_eq!(bytes_of::<&str>(&text.as_str()), expected);
        assert_eq!(bytes_of::<&[u8]>(&text.as_bytes()), expected);
        assert_eq!(bytes_of::<[u8; 16]>(&[3; 16]), Some(&[3; 16][..]));
        assert_eq!(bytes_of::<[u8; 20]>(&[4; 20]), Some(&[4; 20][..]));
        assert_eq!(bytes_of::<[u8; 32]>(&[5; 32]), Some(&[5; 32][..]));

        assert!(ByteString::<u64>::recognize().is_none());
        assert!(ByteString::<[u8; 8]>::recognize().is_none());
        assert!(ByteString::<Vec<i8>>::recognize().is_none());
        assert!(ByteString::<&String>::recognize().is_none());
        assert!(ByteString::<&&str>::recognize().is_none());
        assert!(ByteString::<Box<String>>::recognize().is_none());
        assert!(ByteString::<InlineWord>::recognize().is_none());
    }

    /// A lookup type is the key type only if it is that type: `K: Borrow<Q>`
    /// makes a borrowed key type's lifetimes match too.
    #[test]
    fn same_type_is_the_key_type_itself() {
        let text = "same".to_owned();
        let borrowed: &str = &text;
        let same = SameType::<&str, &str>::recognize().expect("the same type");
        assert_eq!(*same.cast(&borrowed), "same");
        assert!(SameType::<String, String>::recognize().is_some());
        assert!(SameType::<String, str>::recognize().is_none());
        assert!(SameType::<Vec<u8>, [u8]>::recognize().is_none());
        assert!(SameType::<&str, str>::recognize().is_none());
    }
}
