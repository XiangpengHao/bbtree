//! Keys from fuzzer input. An input names a few byte strings, its stems, and
//! each key as a stem and a number after it. Keys of one stem share it as a
//! prefix, as keys in real maps share a path, table or tenant prefix, so
//! nodes strip prefixes (`design/05-fence-key.md`) of the lengths and contents
//! that the input picks, and their inline keys tie where stems agree past
//! them. Numbers come from a small space, so that operations meet keys
//! already in the map, and consecutive numbers make ascending keys.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt::Debug;

use bbtree::{InlineKey, InlineWord};
use libfuzzer_sys::arbitrary::{self, Arbitrary};

/// The byte strings that an input's keys start with.
#[derive(Arbitrary, Debug)]
pub struct Stems(Vec<Vec<u8>>);

impl Stems {
    /// Long enough for keys to share more than an inline key's 8 bytes past
    /// their fences, and across the tree (D50); longer stems only make keys
    /// slower to compare.
    const MAX_LEN: usize = 40;

    /// Stem `index`, modulo their number; empty if there are none.
    fn get(&self, index: u8) -> &[u8] {
        if self.0.is_empty() {
            return &[];
        }
        let stem = &self.0[usize::from(index) % self.0.len()];
        &stem[..stem.len().min(Self::MAX_LEN)]
    }
}

/// A key from the input: a stem and a number after it.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub struct KeyRecipe {
    stem: u8,
    number: u16,
}

impl KeyRecipe {
    /// Small enough for operations to meet keys already in the map, large
    /// enough for every node kind to split. Number `NUMBERS` itself stands
    /// for none: a byte-string key that is its stem alone, a prefix of every
    /// other key of its stem.
    const NUMBERS: u16 = 4096;

    fn number(self) -> u16 {
        self.number % (Self::NUMBERS + 1)
    }

    /// The key `offset` numbers after this one in its stem, wrapping around.
    pub fn after(self, offset: u16) -> Self {
        let numbers = Self::NUMBERS + 1;
        Self {
            stem: self.stem,
            number: (self.number() + offset % numbers) % numbers,
        }
    }

    /// The key as a byte string: its stem, then its number, two bytes
    /// big-endian, if it has one.
    fn byte_string(self, stems: &Stems) -> Vec<u8> {
        let mut bytes = stems.get(self.stem).to_vec();
        if self.number() < Self::NUMBERS {
            bytes.extend_from_slice(&self.number().to_be_bytes());
        }
        bytes
    }

    /// The key as `N` bytes: the first `N - 2` of its stem, padded with
    /// zeros, then its number, big-endian.
    fn fixed_bytes<const N: usize>(self, stems: &Stems) -> [u8; N] {
        let mut bytes = [0; N];
        let stem = stems.get(self.stem);
        let stem_len = stem.len().min(N - 2);
        bytes[..stem_len].copy_from_slice(&stem[..stem_len]);
        bytes[N - 2..].copy_from_slice(&self.number().to_be_bytes());
        bytes
    }
}

/// A key type that the targets make from recipes.
pub trait FuzzKey: Ord + Clone + Debug {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self;

    /// Runs `lookup` through the key's borrowed form: `[u8]` for byte
    /// strings, the key itself for keys without another form. A method, not
    /// a `Borrow` bound on the trait, which would win type inference in
    /// every lookup by the key itself.
    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>);
}

/// A lookup through a borrowed form of keys `K`.
pub trait BorrowedLookup<K> {
    fn look_up<Q: Ord + Debug + ?Sized>(self, key: &Q)
    where
        K: Borrow<Q>;
}

impl FuzzKey for u64 {
    /// Big-endian, the stem's first six bytes are the high ones: each stem
    /// a cluster, whose keys share their high bytes.
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        Self::from_be_bytes(recipe.fixed_bytes(stems))
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up(self);
    }
}

impl FuzzKey for u32 {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        Self::from_be_bytes(recipe.fixed_bytes(stems))
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up(self);
    }
}

impl FuzzKey for Vec<u8> {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        recipe.byte_string(stems)
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up::<[u8]>(self);
    }
}

impl FuzzKey for [u8; 16] {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        recipe.fixed_bytes(stems)
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up::<[u8]>(self);
    }
}

/// A 64-byte key without inline keys: 14 fit a 1 KB node of either kind
/// (D13, D15), so that trees reach three levels from a few hundred entries.
/// Arrays order by their first word, the recipe's; the others follow from it.
pub type WideKey = [u64; 8];

impl FuzzKey for WideKey {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        let first = u64::from_recipe(recipe, stems);
        std::array::from_fn(|index| first ^ index as u64)
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up::<[u64]>(self);
    }
}

/// A byte-string key with its own `InlineKey` impl, as a user's newtype
/// would have: maps with `WithInlineKey` reach its windows through the
/// trait, at the default width, and a lookup by `[u8]`, a form the nodes
/// can't encode for it, searches by `Ord` alone (D19). Its hash tag is its
/// last byte, not its length, which the keys' numbers vary. Its ties compare
/// as its bytes' do, from where keys differ, so its internal nodes keep 4
/// bytes of its inline keys and read the rest again as they move keys (D43).
/// Its check against what every key shares is its bytes' too (D50).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UserKey(Vec<u8>);

impl InlineKey for UserKey {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.inline_key_at(offset)
    }

    fn hash_tag(&self) -> u8 {
        self.0.last().copied().unwrap_or(0)
    }

    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.0.cmp_from(&other.0, equal_len)
    }

    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.0.cmp_prefix(prefix)
    }
}

impl Borrow<[u8]> for UserKey {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl FuzzKey for UserKey {
    fn from_recipe(recipe: KeyRecipe, stems: &Stems) -> Self {
        Self(recipe.byte_string(stems))
    }

    fn look_up_borrowed(&self, lookup: impl BorrowedLookup<Self>) {
        lookup.look_up::<[u8]>(&self.0);
    }
}
