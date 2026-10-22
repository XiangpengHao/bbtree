//! A part of the post as tabs: under the part's overview, a tab for each
//! of its sections, one open at a time, so that a reader sees every
//! section's title at once and reads the one they want. The first section
//! is open until the reader picks another or the address names a heading
//! in another; a pick names its section in the address, so that a link to
//! the page keeps it. An open section ends with a link to the next. The
//! closed sections stay in the page, hidden until found: a find in the page
//! searches them, and opens the one it lands in.

use dioxus::prelude::*;

use crate::outline::PostHeading;
use crate::tab_list::{Tab, TabList, TabPanel};

/// Which of a part's sections is open: owned by the part.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PartTabs {
    part: PostHeading,
    open: Signal<PostHeading>,
}

impl PartTabs {
    /// The tabs of `part`, its first section open until the reader or the
    /// address opens another: a hook.
    pub(crate) fn use_new(part: PostHeading) -> Self {
        let tabs = Self {
            part,
            open: use_signal(|| part.sections().next().unwrap_or(part)),
        };
        use_hook(move || tabs.follow_address());
        tabs
    }

    /// Opens the section that holds the heading the address names, as the
    /// page loads and as the address changes, so that a link to a heading
    /// in a closed section opens it.
    fn follow_address(self) {
        spawn(async move {
            let mut heard = document::eval(
                r#"
                const tell = () => dioxus.send(decodeURIComponent(location.hash.slice(1)));
                addEventListener('hashchange', tell);
                tell();
                "#,
            );
            while let Ok(id) = heard.recv::<String>().await {
                if let Some(heading) = PostHeading::of_id(&id) {
                    self.open_section_of(heading);
                }
            }
        });
    }

    fn open_section_of(mut self, heading: PostHeading) {
        let section = heading.section();
        if section != self.part && section.part() == self.part {
            self.open.set(section);
        }
    }

    /// Opens the section the reader picked, and names it in the address,
    /// without moving the page or adding to the history.
    fn pick(mut self, section: PostHeading) {
        self.open.set(section);
        document::eval(&format!(
            "history.replaceState(history.state, '', '#{}')",
            section.id()
        ));
    }

    fn is_open(self, section: PostHeading) -> bool {
        (self.open)() == section
    }

    /// The section after `section` in the part, if any.
    fn after(self, section: PostHeading) -> Option<PostHeading> {
        self.part
            .sections()
            .skip_while(|other| *other != section)
            .nth(1)
    }

    /// The id of a section's panel, which its tab names.
    fn panel_id(section: PostHeading) -> String {
        format!("{}-panel", section.id())
    }
}

/// The part's tabs, under its overview: a section's title each, whose id
/// is the section's anchor, so that a link to the section lands on them.
#[component]
pub(crate) fn PartTabList(tabs: PartTabs) -> Element {
    let sections = tabs.part.sections();
    rsx! {
        TabList {
            name: tabs.part.title(),
            class: "part-tabs",
            tab_class: "part-tab",
            tabs: sections
                .map(|section| {
                    Tab::new(
                        section.id().to_string(),
                        PartTabs::panel_id(section),
                        rsx! {
                            SectionTitle { section }
                        },
                    )
                })
                .collect::<Vec<_>>(),
            shown: tabs.part.sections().position(|section| tabs.is_open(section)).unwrap_or(0),
            onpick: move |index: usize| {
                if let Some(section) = tabs.part.sections().nth(index) {
                    tabs.pick(section);
                }
            },
        }
    }
}

/// A section's panel, hidden until found while another section is open:
/// the outline lights the section from its top, and it ends with a link to
/// the part's next section.
#[component]
pub(crate) fn SectionPanel(tabs: PartTabs, section: PostHeading, children: Element) -> Element {
    rsx! {
        TabPanel {
            id: PartTabs::panel_id(section),
            tab_id: section.id().to_string(),
            class: "section-panel",
            shown: tabs.is_open(section),
            onreveal: move |_| tabs.open_section_of(section),
            "data-heading": section.id(),
            {children}
            if let Some(next) = tabs.after(section) {
                a { class: "next-section", href: "#{next.id()}",
                    span { class: "next-section-label", "Next" }
                    span { class: "next-section-title",
                        SectionTitle { section: next }
                        span { "aria-hidden": "true", " →" }
                    }
                }
            }
        }
    }
}

/// A section's title as its tab and the link to it show it: a principle's
/// with its rank.
#[component]
fn SectionTitle(section: PostHeading) -> Element {
    rsx! {
        if let Some(number) = section.principle() {
            span { class: "principle-number", "aria-hidden": "true", "{number}" }
        }
        span { {section.title()} }
    }
}
