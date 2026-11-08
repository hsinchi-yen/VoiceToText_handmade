//! Rule-based filler removal, used when LLM polish is off, unavailable, or times out.
//! Implemented in T1.5.

/// Removes Chinese and English filler words (嗯、呃、那個、um、uh…) without changing meaning.
pub fn strip_fillers(text: &str) -> String {
    let _ = text;
    todo!("T1.5")
}
