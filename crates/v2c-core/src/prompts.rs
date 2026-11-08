//! Prompt builders. Content rules: `tasks/plan.md` section 2.
//! `polish` is implemented in T1.5, `translate` in T1.6, vocabulary handling in T1.8.

/// Base ASR initial prompt that biases recognition toward Taiwan Traditional Chinese.
pub const ASR_BASE_PROMPT: &str = "以下是台灣繁體中文與 English 夾雜的口語句子。";

/// Initial prompt for ASR: base prompt plus vocabulary, truncated to `max_chars`.
pub fn asr_initial_prompt(vocabulary: &[String], max_chars: usize) -> String {
    let _ = (vocabulary, max_chars);
    todo!("T1.8")
}

/// System prompt for light polishing.
pub fn polish(vocabulary: &[String]) -> String {
    let _ = vocabulary;
    todo!("T1.5")
}

/// System prompt for Chinese -> English translation.
pub fn translate(vocabulary: &[String]) -> String {
    let _ = vocabulary;
    todo!("T1.6")
}
