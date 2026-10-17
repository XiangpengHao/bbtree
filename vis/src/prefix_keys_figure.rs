//! The post's figure of prefix keys (doc 14, Figure 1): four keys whose
//! first 8 bytes tie, and the 8 bytes past what they share, which differ:
//! why a node skips the bytes its keys share. Wikipedia titles as the
//! benchmarks store them, underscores for spaces; every byte shown is the
//! keys' own.

use dioxus::prelude::*;

use crate::how_it_works::Diagram;

/// Keys a node may hold, and the windows of them a search compares.
struct SharedPrefixExample {
    keys: [&'static str; 4],
}

impl SharedPrefixExample {
    /// The bytes of a key a string node keeps as its prefix key.
    const WIDTH: usize = 8;

    const BRIDGES: Self = Self {
        keys: [
            "List_of_bridges_in_Afghanistan",
            "List_of_bridges_in_Alabama",
            "List_of_bridges_in_Alaska",
            "List_of_bridges_in_Albania",
        ],
    };

    /// How many leading bytes every key shares.
    fn shared_len(&self) -> usize {
        let first = self.keys[0].as_bytes();
        self.keys[1..]
            .iter()
            .map(|key| {
                first
                    .iter()
                    .zip(key.as_bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .min()
            .unwrap_or(first.len())
    }

    /// `key`'s `WIDTH` bytes from `offset`, a dot for each zero byte that
    /// pads a window past the key's end.
    fn window(key: &str, offset: usize) -> String {
        (offset..offset + Self::WIDTH)
            .map(|at| key.as_bytes().get(at).map_or('·', |&byte| byte as char))
            .collect()
    }
}

#[component]
pub(crate) fn PrefixKeysFigure() -> Element {
    let example = SharedPrefixExample::BRIDGES;
    let shared = example.shared_len();
    let width = SharedPrefixExample::WIDTH;
    let shared_text = &example.keys[0][..shared];
    rsx! {
        Diagram {
            caption: rsx! {
                "Four Wikipedia titles. Their first 8 bytes are the same, so a plain prefix decides nothing. They share their first {shared} bytes, "
                code { {shared_text} }
                ": a node whose keys all share them skips them, and keeps the next 8 bytes, which tell the keys apart."
            },
            div { class: "prefix-keys",
                span { class: "prefix-keys-head prefix-keys-key", "key" }
                span { class: "prefix-keys-head", "first 8 bytes" }
                span { class: "prefix-keys-head", "8 bytes past the shared {shared}" }
                for key in example.keys {
                    span { key: "{key}", class: "prefix-keys-key data",
                        span { class: "prefix-keys-shared", "{&key[..shared]}" }
                        span { class: "prefix-keys-window", "{&key[shared..key.len().min(shared + width)]}" }
                        "{&key[key.len().min(shared + width)..]}"
                    }
                    span { class: "byte-chip tied data", {SharedPrefixExample::window(key, 0)} }
                    span { class: "byte-chip apart data", {SharedPrefixExample::window(key, shared)} }
                }
                span { class: "prefix-keys-foot prefix-keys-key" }
                span { class: "prefix-keys-foot", "all tie: compare in full" }
                span { class: "prefix-keys-foot", "all differ: decided in a register" }
            }
        }
    }
}
