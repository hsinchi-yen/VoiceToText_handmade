//! Text model interface used for polishing and Chinese-to-English translation.
//! Implementations: OpenAI-compatible (LM Studio / self-hosted, T1.5), OpenAI, Gemini, Claude (T1.7).

use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Short stable name for logs and settings, e.g. `lmstudio`, `claude`.
    fn name(&self) -> &str;

    /// Single-turn completion: `system` instructions plus the user's text. Returns only the model's text.
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}
