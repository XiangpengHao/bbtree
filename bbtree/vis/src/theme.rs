//! The post's theme, light or dark: the reader's system's until the reader
//! picks one, which the browser keeps (`localStorage`) and `index.html`
//! sets on the page before its first paint (`data-theme`). The page's
//! colors follow from it in CSS (`light-dark()` in `main.css`); its
//! figures, whose style is their own, take it from an attribute
//! (`data-theme`; `post_figure.rs`). The explorer stays light.

use dioxus::prelude::*;

use crate::icons::{Icon, IconGlyph};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn slug(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn of_slug(slug: &str) -> Option<Self> {
        match slug {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

/// The theme the reader picked, if any, and the system's: owned by the
/// post, which provides it to its figures.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PostTheme {
    picked: Signal<Option<Theme>>,
    system: Signal<Theme>,
}

impl PostTheme {
    const STORAGE_KEY: &'static str = "bbtree-theme";

    /// The post's theme, as the page started with it: a hook.
    pub(crate) fn use_new() -> Self {
        let theme = Self {
            picked: use_signal(|| None),
            system: use_signal(|| Theme::Light),
        };
        use_hook(move || theme.listen());
        use_context_provider(|| theme)
    }

    /// The theme of the post's context, for its figures.
    pub(crate) fn of_context() -> Self {
        use_context()
    }

    /// Reads the theme `index.html` set and the system's, and hears the
    /// system's change.
    fn listen(self) {
        let (mut picked, mut system) = (self.picked, self.system);
        spawn(async move {
            let mut heard = document::eval(
                r#"
                const query = matchMedia('(prefers-color-scheme: dark)');
                dioxus.send([document.documentElement.dataset.theme ?? '', query.matches]);
                query.addEventListener('change', (event) => dioxus.send(['unchanged', event.matches]));
                "#,
            );
            while let Ok((chosen, dark)) = heard.recv::<(String, bool)>().await {
                if chosen != "unchanged" {
                    picked.set(Theme::of_slug(&chosen));
                }
                system.set(if dark { Theme::Dark } else { Theme::Light });
            }
        });
    }

    fn shown(self) -> Theme {
        (self.picked)().unwrap_or((self.system)())
    }

    /// Picks the theme not shown, for the page and the next visit.
    fn switch(self) {
        let next = self.shown().other();
        let mut picked = self.picked;
        picked.set(Some(next));
        let slug = next.slug();
        let key = Self::STORAGE_KEY;
        document::eval(&format!(
            "document.documentElement.dataset.theme = '{slug}'; \
             try {{ localStorage.setItem('{key}', '{slug}'); }} catch (_) {{}}"
        ));
    }

    /// The theme the reader picked, if any, for a figure to take
    /// (`data-theme`, `vis/figures/src/svg.rs`), so that otherwise it
    /// follows the system as the page does.
    pub(crate) fn picked_slug(self) -> Option<&'static str> {
        (self.picked)().map(Theme::slug)
    }
}

/// The header's switch between the themes, showing the one it switches to.
#[component]
pub(crate) fn ThemeSwitch(theme: PostTheme) -> Element {
    let next = theme.shown().other();
    let (label, icon) = match next {
        Theme::Light => ("Switch to the light theme", Icon::Sun),
        Theme::Dark => ("Switch to the dark theme", Icon::Moon),
    };
    rsx! {
        button {
            class: "toolbar-icon-link theme-switch",
            r#type: "button",
            title: label,
            "aria-label": label,
            onclick: move |_| theme.switch(),
            IconGlyph { icon }
        }
    }
}
