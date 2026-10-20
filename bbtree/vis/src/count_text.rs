//! Counts as people read them.

use std::fmt;

/// A count with its digits grouped in threes: 10,000,000.
pub(crate) struct Group(pub(crate) usize);

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.0.to_string();
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_group_their_digits() {
        assert_eq!(Group(7).to_string(), "7");
        assert_eq!(Group(1000).to_string(), "1,000");
        assert_eq!(Group(10_000_000).to_string(), "10,000,000");
    }
}
