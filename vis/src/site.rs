//! The site's two pages, from one build: the explorer and the post. Which
//! one the address names, the header both draw with the tabs between them,
//! and the credit. A tab is a link, so moving between the pages loads the
//! other afresh: the explorer alone writes the browser's history
//! (`shared_link.rs`).

use dioxus::prelude::*;

use crate::icons::{Icon, IconGlyph};

pub(crate) const REPOSITORY: &str = "https://github.com/XiangpengHao/bbtree";
pub(crate) const AUTHOR_PAGE: &str = "https://xiangpeng.systems";

/// One of the site's pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SitePage {
    /// Look keys up in a tree of the reader's choosing.
    Explorer,
    /// The post, with its figures.
    Post,
}

impl SitePage {
    const ALL: [SitePage; 2] = [SitePage::Explorer, SitePage::Post];

    /// The page the address names: the post at `post/`, the explorer
    /// anywhere else, so every link made before the post still opens it.
    pub(crate) fn current() -> Self {
        let route = history().current_route();
        let path = route.split(['?', '#']).next().unwrap_or_default();
        if path.trim_end_matches('/') == SitePage::Post.path().trim_end_matches('/') {
            SitePage::Post
        } else {
            SitePage::Explorer
        }
    }

    /// The page's address.
    pub(crate) fn href(self) -> String {
        let base = history().current_prefix().unwrap_or_default();
        format!("{base}{}", self.path())
    }

    /// The page's path under the site's base path.
    fn path(self) -> &'static str {
        match self {
            SitePage::Explorer => "/",
            SitePage::Post => "/post/",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SitePage::Explorer => "Explorer",
            SitePage::Post => "Post",
        }
    }
}

/// The bar atop each page: the wordmark, the tabs between the pages, what
/// the page puts in it (`children`), and the repository's mark.
#[component]
pub(crate) fn SiteHeader(current: SitePage, children: Element) -> Element {
    rsx! {
        header { class: "toolbar",
            // The post's title is its page's heading.
            if current == SitePage::Explorer {
                h1 { class: "wordmark", "bbtree" }
            } else {
                p { class: "wordmark", "bbtree" }
            }
            nav { class: "site-tabs", "aria-label": "Pages",
                for page in SitePage::ALL {
                    a {
                        class: "site-tab",
                        href: page.href(),
                        "aria-current": if page == current { "page" } else { "false" },
                        {page.label()}
                    }
                }
            }
            {children}
            div { class: "toolbar-end",
                a {
                    class: "toolbar-icon-link",
                    href: REPOSITORY,
                    target: "_blank",
                    rel: "noopener",
                    title: "bbtree on GitHub",
                    "aria-label": "bbtree on GitHub",
                    IconGlyph { icon: Icon::GitHub }
                }
            }
        }
    }
}

/// Who made the site.
#[component]
pub(crate) fn Credit() -> Element {
    rsx! {
        footer { class: "credit",
            "Created with care by "
            a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng Hao" }
        }
    }
}
