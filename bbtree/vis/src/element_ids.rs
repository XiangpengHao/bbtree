//! Ids for the elements the browser finds by id, unique to the component
//! that draws them: a page may draw a panel more than once, as the post
//! draws one in each figure.

use dioxus::core::current_scope_id;

/// What tells one drawn component's element ids from another's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ElementIds(usize);

impl ElementIds {
    /// The ids of the component drawing now.
    pub(crate) fn of_current() -> Self {
        Self(current_scope_id().0)
    }

    /// The id of the element `name` names.
    pub(crate) fn id(self, name: &str) -> String {
        format!("{name}-{}", self.0)
    }
}
