//! A row of tabs over their panels, as the WAI-ARIA tabs pattern has them:
//! a click or an arrow key picks a tab, and only the picked tab takes the
//! keyboard's focus in turn, so that Tab moves on into its panel. The owner
//! keeps which tab is picked and draws every panel, under the id its tab
//! names: the others are hidden until found, so that the browser's find
//! searches them too.

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

/// A tab's panel, in the page whether or not its tab is picked: while it
/// isn't, the panel is hidden until found. When a find or a link lands in
/// it, the browser shows it and `onreveal` hears so, for the owner to pick
/// its tab; the tabs' other panels hide at once, as that pick would hide
/// them, so that the browser brings the match into view where it stays.
#[component]
pub(crate) fn TabPanel(
    id: String,
    /// The id of the panel's tab, which names it.
    tab_id: String,
    class: &'static str,
    shown: bool,
    onreveal: EventHandler,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let listened = id.clone();
    rsx! {
        div {
            id,
            class,
            role: "tabpanel",
            "aria-labelledby": tab_id,
            // Dioxus takes `hidden` for a flag and drops any other value;
            // an attribute's name is case-insensitive, so this spelling
            // keeps it.
            "HIDDEN": (!shown).then_some("until-found"),
            onmounted: move |_| {
                let id = listened.clone();
                spawn(async move {
                    let mut heard = document::eval(
                        r#"
                        const panel = document.getElementById(await dioxus.recv());
                        panel?.addEventListener('beforematch', (event) => {
                            if (event.target !== panel) return;
                            const tab = document.getElementById(panel.getAttribute('aria-labelledby'));
                            for (const other of tab?.closest('[role=tablist]')?.querySelectorAll('[role=tab]') ?? []) {
                                const sibling = document.getElementById(other.getAttribute('aria-controls'));
                                if (sibling !== panel) sibling?.setAttribute('hidden', 'until-found');
                            }
                            dioxus.send(true);
                        });
                        "#,
                    );
                    let _ = heard.send(id);
                    while heard.recv::<bool>().await.is_ok() {
                        onreveal.call(());
                    }
                });
            },
            ..attributes,
            {children}
        }
    }
}
