//! serde impls for [`unsync::BBTreeMap`] and [`unsync::BBTreeSet`], matching
//! std's `BTreeMap`/`BTreeSet`: a map serializes as a map, a set as a
//! sequence, both in key order.

use core::{fmt, marker::PhantomData};

use serde::{
    de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor},
    ser::{Serialize, Serializer},
};

use crate::{
    Allocator, InlineKeyStrategy,
    unsync::{BBTreeMap, BBTreeSet},
};

impl<K: Serialize, V: Serialize, S: InlineKeyStrategy<K>, A: Allocator + Clone> Serialize
    for BBTreeMap<K, V, S, A>
{
    fn serialize<Se: Serializer>(&self, serializer: Se) -> Result<Se::Ok, Se::Error> {
        serializer.collect_map(self)
    }
}

impl<T: Serialize, S: InlineKeyStrategy<T>, A: Allocator + Clone> Serialize for BBTreeSet<T, S, A> {
    fn serialize<Se: Serializer>(&self, serializer: Se) -> Result<Se::Ok, Se::Error> {
        serializer.collect_seq(self)
    }
}

impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Deserialize<'de> for BBTreeMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Visitor<'de> for MapVisitor<K, V> {
            type Value = BBTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut map = BBTreeMap::new();
                while let Some((key, value)) = access.next_entry()? {
                    map.insert(key, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

impl<'de, T: Deserialize<'de> + Ord> Deserialize<'de> for BBTreeSet<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SetVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de> + Ord> Visitor<'de> for SetVisitor<T> {
            type Value = BBTreeSet<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut set = BBTreeSet::new();
                while let Some(value) = access.next_element()? {
                    set.insert(value);
                }
                Ok(set)
            }
        }

        deserializer.deserialize_seq(SetVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        collections::{BTreeMap, BTreeSet},
        format,
        string::String,
        vec::Vec,
    };

    use crate::unsync::{BBTreeMap, BBTreeSet};

    /// Enough keys for several leaves, so a round trip crosses node boundaries.
    const KEY_COUNT: u32 = 2_000;

    #[test]
    fn map_serializes_like_std_btree_map() {
        let pairs: Vec<(u32, String)> = (0..KEY_COUNT)
            .rev()
            .map(|key| (key, format!("v{key}")))
            .collect();
        let ours: BBTreeMap<u32, String> = pairs.iter().cloned().collect();
        let std_map: BTreeMap<u32, String> = pairs.into_iter().collect();

        let json = serde_json::to_string(&ours).unwrap();
        assert_eq!(json, serde_json::to_string(&std_map).unwrap());

        let back: BBTreeMap<u32, String> = serde_json::from_str(&json).unwrap();
        assert!(back.iter().eq(std_map.iter()));
    }

    #[test]
    fn set_serializes_like_std_btree_set() {
        let ours: BBTreeSet<u32> = (0..KEY_COUNT).rev().collect();
        let std_set: BTreeSet<u32> = (0..KEY_COUNT).collect();

        let json = serde_json::to_string(&ours).unwrap();
        assert_eq!(json, serde_json::to_string(&std_set).unwrap());

        let back: BBTreeSet<u32> = serde_json::from_str(&json).unwrap();
        assert!(back.iter().eq(std_set.iter()));
    }

    #[test]
    fn duplicate_keys_keep_the_last_value_like_std() {
        let json = r#"{"1":"a","1":"b"}"#;
        let ours: BBTreeMap<u32, String> = serde_json::from_str(json).unwrap();
        let std_map: BTreeMap<u32, String> = serde_json::from_str(json).unwrap();
        assert!(ours.iter().eq(std_map.iter()));
    }
}
