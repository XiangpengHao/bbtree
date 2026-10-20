//! The post's Rust code, highlighted: a block of code split into tokens
//! (`RustTokens`), each drawn in its kind's color (`.rust-*` in
//! `main.css`). Enough of Rust's lexical grammar for the post's snippets:
//! comments, strings and characters, numbers, attributes, keywords, and
//! names told apart by case and by what follows them. Highlighted as the
//! page draws, so the code stays text in the source and the page.

use dioxus::prelude::*;

/// What a token is, for its color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RustToken {
    Plain,
    Comment,
    Keyword,
    String,
    Number,
    /// An attribute, `#[...]`, whole.
    Attribute,
    /// A macro's name with its `!`.
    Macro,
    /// A name called: followed by `(`.
    Function,
    /// A type: a capitalized name, or a primitive's.
    Type,
}

impl RustToken {
    fn class(self) -> &'static str {
        match self {
            Self::Plain => "rust-plain",
            Self::Comment => "rust-comment",
            Self::Keyword => "rust-keyword",
            Self::String => "rust-string",
            Self::Number => "rust-number",
            Self::Attribute => "rust-attribute",
            Self::Macro => "rust-macro",
            Self::Function => "rust-function",
            Self::Type => "rust-type",
        }
    }
}

/// The tokens of a piece of Rust, in order, each the text it spans: they
/// add up to the code, every byte in one.
struct RustTokens<'a> {
    rest: &'a str,
}

impl<'a> RustTokens<'a> {
    const KEYWORDS: [&'static str; 32] = [
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "static", "struct", "trait", "true", "unsafe", "while",
    ];
    const PRIMITIVES: [&'static str; 17] = [
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8",
        "u16", "u32", "u64", "u128", "usize",
    ];

    fn new(code: &'a str) -> Self {
        Self { rest: code }
    }

    /// How many leading bytes of the rest the next token takes, and its
    /// kind.
    fn next_token(&self) -> (RustToken, usize) {
        let rest = self.rest;
        let first = rest.chars().next().unwrap_or(' ');
        if rest.starts_with("//") {
            (RustToken::Comment, rest.find('\n').unwrap_or(rest.len()))
        } else if first == '"' {
            (RustToken::String, Self::string_len(rest))
        } else if first == '\'' {
            Self::quote(rest)
        } else if rest.starts_with("#[") {
            (
                RustToken::Attribute,
                rest.find(']').map_or(rest.len(), |end| end + 1),
            )
        } else if first.is_ascii_digit() {
            (RustToken::Number, Self::number_len(rest))
        } else if first.is_alphabetic() || first == '_' {
            self.name()
        } else {
            let len = rest
                .find(|c: char| c.is_alphanumeric() || matches!(c, '_' | '"' | '\'' | '#' | '/'))
                .filter(|&len| len > 0)
                .unwrap_or(first.len_utf8());
            (RustToken::Plain, len)
        }
    }

    /// A string literal's length, to its closing quote past escapes.
    fn string_len(rest: &str) -> usize {
        let mut escaped = false;
        for (at, c) in rest.char_indices().skip(1) {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => return at + 1,
                _ => escaped = false,
            }
        }
        rest.len()
    }

    /// A number's length: digits, letters (`0x`, suffixes), `_`, and a
    /// `.` only before a digit, so that `4..8` is two numbers.
    fn number_len(rest: &str) -> usize {
        let bytes = rest.as_bytes();
        (0..bytes.len())
            .find(|&at| {
                let byte = bytes[at];
                let decimal_point =
                    byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit);
                !(byte.is_ascii_alphanumeric() || byte == b'_' || decimal_point)
            })
            .unwrap_or(rest.len())
    }

    /// A character literal, `'x'` or `'\n'`, or else a lifetime, `'a`.
    fn quote(rest: &str) -> (RustToken, usize) {
        let mut chars = rest.char_indices().skip(1);
        match (chars.next(), chars.next()) {
            (Some((_, '\\')), _) => {
                let end = rest[2..].find('\'').map_or(rest.len(), |at| at + 3);
                (RustToken::String, end)
            }
            (Some(_), Some((at, '\''))) => (RustToken::String, at + 1),
            _ => {
                let len = rest[1..]
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .map_or(rest.len(), |at| at + 1);
                (RustToken::Type, len)
            }
        }
    }

    /// A name, by what it is and what follows it.
    fn name(&self) -> (RustToken, usize) {
        let rest = self.rest;
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..len];
        let after = &rest[len..];
        if Self::KEYWORDS.contains(&name) {
            (RustToken::Keyword, len)
        } else if after.starts_with('!') {
            (RustToken::Macro, len + 1)
        } else if Self::PRIMITIVES.contains(&name) || name.starts_with(|c: char| c.is_uppercase()) {
            (RustToken::Type, len)
        } else if after.starts_with('(') || after.starts_with("::<") {
            (RustToken::Function, len)
        } else {
            (RustToken::Plain, len)
        }
    }
}

impl<'a> Iterator for RustTokens<'a> {
    type Item = (RustToken, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let (kind, len) = self.next_token();
        let (text, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some((kind, text))
    }
}

/// A block of Rust, highlighted.
#[component]
pub(crate) fn RustCode(code: &'static str) -> Element {
    rsx! {
        pre { class: "rust-code",
            code {
                for (index , (kind , text)) in RustTokens::new(code).enumerate() {
                    span { key: "{index}", class: kind.class(), {text} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(code: &str) -> Vec<(RustToken, &str)> {
        RustTokens::new(code)
            .filter(|(kind, _)| *kind != RustToken::Plain)
            .collect()
    }

    #[test]
    fn tokens_add_up_to_the_code() {
        let code = "#[derive(Ord)]\nfn f<'a>(x: &'a [u8; 16]) -> Self { let c = '\\n'; g!(\"a\\\"b\") } // done";
        let joined: String = RustTokens::new(code).map(|(_, text)| text).collect();
        assert_eq!(joined, code);
    }

    #[test]
    fn tokens_are_told_apart() {
        use RustToken::*;
        assert_eq!(
            kinds("pub fn child_index(keys: &[u16; 32]) -> usize { keys.count_ones() as usize }"),
            vec![
                (Keyword, "pub"),
                (Keyword, "fn"),
                (Function, "child_index"),
                (Type, "u16"),
                (Number, "32"),
                (Type, "usize"),
                (Function, "count_ones"),
                (Keyword, "as"),
                (Type, "usize"),
            ]
        );
        assert_eq!(
            kinds("#[derive(Ord)] let s = \"x\"; // one\nlet c = 'y'; f::<T>(0x9e_37);"),
            vec![
                (Attribute, "#[derive(Ord)]"),
                (Keyword, "let"),
                (String, "\"x\""),
                (Comment, "// one"),
                (Keyword, "let"),
                (String, "'y'"),
                (Function, "f"),
                (Type, "T"),
                (Number, "0x9e_37"),
            ]
        );
        assert_eq!(kinds("&'a str"), vec![(Type, "'a"), (Type, "str")]);
        assert_eq!(kinds("x[4..8]"), vec![(Number, "4"), (Number, "8")]);
        assert_eq!(kinds("1.5"), vec![(Number, "1.5")]);
        assert_eq!(
            kinds("assert!(true)"),
            vec![(Macro, "assert!"), (Keyword, "true")]
        );
    }
}
