//! Helpers that keep user text out of logs.

use std::fmt;

/// Formats a piece of user text for `Debug` output without revealing it.
pub struct Redacted<'a>(pub &'a str);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted {} chars>", self.0.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_shows_length_not_content() {
        let s = format!("{:?}", Redacted("這個 meeting"));
        assert_eq!(s, "<redacted 10 chars>");
        assert!(!s.contains("meeting"));
    }
}
