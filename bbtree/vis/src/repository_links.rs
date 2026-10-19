//! Links to the code of the maps and systems a chart measured, those
//! outside std, at the end of its note.

use dioxus::prelude::*;

/// "Code: " and each name, a link to its repository, in a chart's note.
#[component]
pub(crate) fn RepositoryLinks(links: Vec<(String, &'static str)>) -> Element {
    let count = links.len();
    rsx! {
        if count > 0 {
            " Code: "
            for (index, (name, url)) in links.into_iter().enumerate() {
                a { href: url, target: "_blank", rel: "noopener", {name} }
                if index + 1 < count { ", " } else { "." }
            }
        }
    }
}
