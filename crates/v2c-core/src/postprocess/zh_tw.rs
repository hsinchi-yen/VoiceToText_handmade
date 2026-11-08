//! Simplified -> Traditional Chinese (Taiwan phrasing). English is left unchanged.
//! Implemented in T1.1 (verify the `zhconv` crate first; fall back to an OpenCC binding).

/// Converts Chinese text to Taiwan Traditional Chinese, leaving English untouched.
pub fn to_zh_tw(text: &str) -> String {
    let _ = text;
    todo!("T1.1")
}
