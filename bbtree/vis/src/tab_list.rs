//! A row of tabs over one panel, as the WAI-ARIA tabs pattern has them: a
//! click or an arrow key picks a tab, and only the picked tab takes the
//! keyboard's focus in turn, so that Tab moves on into its panel. The owner
//! keeps which tab is picked and draws the panel, under the id its tab
//! names.

use std::rc::Rc;

use dioxus::prelude::*;

/// A tab: its button's id, its panel's id, and its label.
#[derive(Clone, PartialEq)]
pub(crate) struct Tab {
    id: String,
    panel_id: String,
    label: Element,
}

impl Tab {
    pub(crate) fn new(id: String, panel_id: String, label: Element) -> Self {
        Self {
            id,
            panel_id,
            label,
        }
    }
}

/// The tabs, the one at `shown` picked; `onpick` hears the index of the
/// tab the reader picks.
#[component]
pub(crate) fn TabList(
    /// What the tabs pick between, as assistive technology names it.
    name: &'static str,
    class: &'static str,
    tab_class: &'static str,
    tabs: Vec<Tab>,
    shown: usize,
    onpick: EventHandler<usize>,
) -> Element {
    let ids: Rc<[String]> = tabs.iter().map(|tab| tab.id.clone()).collect();
    rsx! {
        div { class, role: "tablist", "aria-label": name,
            for (index , tab) in tabs.into_iter().enumerate() {
                button {
                    key: "{tab.id}",
                    id: tab.id,
                    class: tab_class,
                    r#type: "button",
                    role: "tab",
                    "aria-selected": if index == shown { "true" } else { "false" },
                    "aria-controls": tab.panel_id,
                    tabindex: if index == shown { "0" } else { "-1" },
                    onclick: move |_| onpick.call(index),
                    onkeydown: {
                        let ids = ids.clone();
                        move |evt: KeyboardEvent| {
                            let next = match evt.key() {
                                Key::ArrowRight => (index + 1) % ids.len(),
                                Key::ArrowLeft => (index + ids.len() - 1) % ids.len(),
                                _ => return,
                            };
                            evt.prevent_default();
                            onpick.call(next);
                            document::eval(&format!("document.getElementById('{}')?.focus()", ids[next]));
                        }
                    },
                    {tab.label}
                }
            }
        }
    }
}
