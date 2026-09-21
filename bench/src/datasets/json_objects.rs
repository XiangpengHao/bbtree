//! The objects of real JSON documents. Each becomes one tiny map that holds
//! the object's keys, inserted in document order as serde_json does.

use std::collections::HashMap;

use super::{DataDirectory, DataFile};
use crate::operations::{ManyMapsKeys, PicksPerMap};

/// The objects of six JSON documents (`DataFile::JsonObjectKeys`), with each
/// key as an index into their vocabulary (`DataFile::JsonKeyVocabulary`).
pub(crate) struct JsonObjectCorpus {
    vocabulary: Vec<String>,
    /// Each object's keys, in document order.
    objects: PicksPerMap,
}

impl JsonObjectCorpus {
    pub(crate) fn load(directory: &DataDirectory) -> Result<Self, String> {
        let vocabulary = directory.read(DataFile::JsonKeyVocabulary)?;
        let objects = directory.read(DataFile::JsonObjectKeys)?;
        Self::parse(&vocabulary, &objects)
    }

    /// Parses the vocabulary, a `key<TAB>count` line per key, and the
    /// objects, a line per object with its keys tab-separated (an empty line
    /// is an empty object), then checks the counts against the objects.
    fn parse(vocabulary_text: &str, objects_text: &str) -> Result<Self, String> {
        // Both files escape `\`, tab, LF and CR with a backslash; the pinned
        // files have no such key, so there is nothing to unescape.
        if vocabulary_text.contains('\\') || objects_text.contains('\\') {
            return Err("JSON keys with escapes aren't supported".to_owned());
        }
        let mut vocabulary = Vec::new();
        let mut counts = Vec::new();
        for line in vocabulary_text.lines() {
            let (key, count) = (line.split_once('\t'))
                .ok_or_else(|| format!("vocabulary line `{line}` has no count"))?;
            let count: usize =
                (count.parse()).map_err(|_| format!("vocabulary line `{line}` has no count"))?;
            vocabulary.push(key.to_owned());
            counts.push(count);
        }
        let mut picks_by_key: HashMap<&str, u16> = HashMap::new();
        for (index, key) in vocabulary.iter().enumerate() {
            let pick = u16::try_from(index).map_err(|_| "the vocabulary has over 65536 keys")?;
            if picks_by_key.insert(key, pick).is_some() {
                return Err(format!("the vocabulary lists `{key}` twice"));
            }
        }

        let mut objects = PicksPerMap::default();
        let mut object_picks = Vec::new();
        for line in objects_text.lines() {
            object_picks.clear();
            for key in line.split('\t').filter(|_| !line.is_empty()) {
                let pick = (picks_by_key.get(key))
                    .ok_or_else(|| format!("object key `{key}` isn't in the vocabulary"))?;
                object_picks.push(*pick);
            }
            objects.push_map(&object_picks);
        }
        if objects.pick_counts(vocabulary.len()) != counts {
            return Err("the vocabulary's counts don't match the objects".to_owned());
        }
        Ok(Self {
            vocabulary,
            objects,
        })
    }

    /// The keys of about `map_count` tiny maps: the corpus, repeated until it
    /// has at least that many objects.
    pub(crate) fn repeated_to(self, map_count: usize) -> ManyMapsKeys<String> {
        let copies = map_count.div_ceil(self.objects.map_count());
        let mut maps = PicksPerMap::with_capacity(
            copies * self.objects.map_count(),
            copies * self.objects.pick_count(),
        );
        for _ in 0..copies {
            for object in self.objects.iter() {
                maps.push_map(object);
            }
        }
        ManyMapsKeys::new(self.vocabulary, maps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOCABULARY: &str = "id\t3\nname\t2\nurl\t1\n";
    const OBJECTS: &str = "id\tname\n\nid\nurl\tid\tname\n";

    #[test]
    fn objects_keep_their_keys_in_document_order() {
        let corpus = JsonObjectCorpus::parse(VOCABULARY, OBJECTS).unwrap();
        let objects: Vec<&[u16]> = corpus.objects.iter().collect();
        assert_eq!(objects, [&[0, 1][..], &[], &[0], &[2, 0, 1]]);
    }

    #[test]
    fn the_corpus_repeats_to_reach_the_map_count() {
        let corpus = JsonObjectCorpus::parse(VOCABULARY, OBJECTS).unwrap();
        let keys = corpus.repeated_to(10);
        assert_eq!(keys.map_count(), 12);
        assert_eq!(keys.entry_count(), 18);
    }

    #[test]
    fn keys_outside_the_vocabulary_are_refused() {
        assert!(JsonObjectCorpus::parse(VOCABULARY, "id\tcolor\n").is_err());
    }

    #[test]
    fn counts_must_match_the_objects() {
        assert!(JsonObjectCorpus::parse(VOCABULARY, "id\tname\n").is_err());
    }
}
