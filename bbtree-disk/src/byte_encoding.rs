//! How an index file keeps keys and values of the caller's types: as bytes.
//! [`ToBytes`] writes them, [`FromBytes`] reads them back, borrowing the
//! file's bytes where it can.
//!
//! An index orders its keys by their bytes, as byte strings, so a key
//! type's bytes order as the type does: text as its UTF-8, integers
//! big-endian, signed ones with the sign bit flipped. A value's bytes need
//! only read back.

/// A key or a value an index file keeps as bytes. For a key, the bytes
/// order as the keys do: `a < b` exactly when `a.to_bytes() < b.to_bytes()`
/// as byte strings, and a borrowed form of a key (`str` for `String`) has
/// the key's bytes.
pub trait ToBytes {
    /// The bytes, borrowed where the value holds them.
    fn to_bytes(&self) -> impl AsRef<[u8]>;
}

/// A key or a value read back from the bytes an index file keeps for it,
/// as [`ToBytes`] wrote them.
pub trait FromBytes {
    /// What reading gives back, borrowing the file's bytes where it can:
    /// `&str` for `String`, `&[u8]` for `Vec<u8>`, the number for an
    /// integer.
    type View<'a>;

    /// The value of `bytes`.
    ///
    /// # Panics
    ///
    /// If `bytes` are not what [`ToBytes`] writes for the type, which only
    /// a damaged file holds.
    fn from_bytes(bytes: &[u8]) -> Self::View<'_>;
}

impl<T: ToBytes + ?Sized> ToBytes for &T {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        (**self).to_bytes()
    }
}

/// Its UTF-8 bytes, which order as the text does.
impl ToBytes for str {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        self.as_bytes()
    }
}

impl ToBytes for String {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        self.as_bytes()
    }
}

impl FromBytes for String {
    type View<'a> = &'a str;

    fn from_bytes(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).expect("text an index file keeps is UTF-8")
    }
}

impl ToBytes for [u8] {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        self
    }
}

impl ToBytes for Vec<u8> {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        self.as_slice()
    }
}

impl FromBytes for Vec<u8> {
    type View<'a> = &'a [u8];

    fn from_bytes(bytes: &[u8]) -> &[u8] {
        bytes
    }
}

impl<const N: usize> ToBytes for [u8; N] {
    fn to_bytes(&self) -> impl AsRef<[u8]> {
        self.as_slice()
    }
}

impl<const N: usize> FromBytes for [u8; N] {
    type View<'a> = &'a [u8; N];

    fn from_bytes(bytes: &[u8]) -> &[u8; N] {
        bytes
            .try_into()
            .expect("an array an index file keeps has its length")
    }
}

/// Unsigned integers: their big-endian bytes, which order as they do.
macro_rules! unsigned_bytes {
    ($($unsigned:ty),*) => {$(
        impl ToBytes for $unsigned {
            fn to_bytes(&self) -> impl AsRef<[u8]> {
                self.to_be_bytes()
            }
        }

        impl FromBytes for $unsigned {
            type View<'a> = $unsigned;

            fn from_bytes(bytes: &[u8]) -> $unsigned {
                let bytes = bytes.try_into().expect("an integer an index file keeps has its width");
                <$unsigned>::from_be_bytes(bytes)
            }
        }
    )*};
}

unsigned_bytes!(u8, u16, u32, u64, u128);

/// Signed integers: their big-endian bytes with the sign bit flipped, which
/// order as they do, negatives first.
macro_rules! signed_bytes {
    ($($signed:ty: $unsigned:ty),*) => {$(
        impl ToBytes for $signed {
            fn to_bytes(&self) -> impl AsRef<[u8]> {
                (self.cast_unsigned() ^ <$signed>::MIN.cast_unsigned()).to_be_bytes()
            }
        }

        impl FromBytes for $signed {
            type View<'a> = $signed;

            fn from_bytes(bytes: &[u8]) -> $signed {
                (<$unsigned>::from_bytes(bytes) ^ <$signed>::MIN.cast_unsigned()).cast_signed()
            }
        }
    )*};
}

signed_bytes!(i8: u8, i16: u16, i32: u32, i64: u64, i128: u128);

#[cfg(test)]
mod tests {
    use super::{FromBytes, ToBytes};

    /// Keys' bytes order as the keys do, and read back as them.
    fn orders_and_reads_back<T>(sorted: &[T])
    where
        T: ToBytes + for<'a> FromBytes<View<'a> = T> + PartialEq + std::fmt::Debug,
    {
        for pair in sorted.windows(2) {
            assert!(
                pair[0].to_bytes().as_ref() < pair[1].to_bytes().as_ref(),
                "{pair:?}"
            );
        }
        for value in sorted {
            assert_eq!(T::from_bytes(value.to_bytes().as_ref()), *value);
        }
    }

    #[test]
    fn integers_order_as_their_bytes() {
        orders_and_reads_back(&[0_u64, 1, 255, 256, u64::MAX]);
        orders_and_reads_back(&[i64::MIN, -256, -1, 0, 1, 255, i64::MAX]);
        orders_and_reads_back(&[i8::MIN, -1, 0, 1, i8::MAX]);
        orders_and_reads_back(&[i128::MIN, -1, 0, i128::MAX]);
    }

    #[test]
    fn text_orders_as_its_bytes() {
        let sorted = ["", "a", "ab", "b", "é", "中"];
        for pair in sorted.windows(2) {
            assert!(pair[0].to_bytes().as_ref() < pair[1].to_bytes().as_ref());
            assert!(pair[0] < pair[1]);
        }
        let text = String::from("héllo");
        assert_eq!(String::from_bytes(text.to_bytes().as_ref()), "héllo");
    }
}
