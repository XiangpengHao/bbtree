//! The site's two pages, from one build: the explorer and the post. Which
//! one the address names, the site's root sending the reader on to the
//! post, the header both draw with the tabs between them, and the credit. A tab is a link, so moving between the pages loads the
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

    /// The page the address names: the post at `post/`, the explorer at
    /// `explorer/` and anywhere else but the root; `None` at the root,
    /// which isn't a page ([`SiteRoot`]).
    pub(crate) fn current() -> Option<Self> {
        let route = history().current_route();
        let path = route.split(['?', '#']).next().unwrap_or_default();
        match path.trim_end_matches('/') {
            "" => None,
            path if path == SitePage::Post.path().trim_end_matches('/') => Some(SitePage::Post),
            _ => Some(SitePage::Explorer),
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
            SitePage::Explorer => "/explorer/",
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

/// The site's root, which sends the reader on: to the explorer, if the
/// address carries an explorer's link from when the explorer was the root;
/// to the post otherwise. The static host does the same without loading
/// the site (`lay-out-site.sh`); this is for any host that serves the
/// site's page at the root.
#[component]
pub(crate) fn SiteRoot() -> Element {
    use_hook(|| {
        let route = history().current_route();
        let query = route
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_default();
        let to = if query.is_empty() {
            SitePage::Post.href()
        } else {
            format!("{}?{query}", SitePage::Explorer.href())
        };
        let eval = document::eval("location.replace(await dioxus.recv());");
        let _ = eval.send(to);
    });
    rsx! {}
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
