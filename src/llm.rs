//! # LLM Adapter Surface
//!
//! Provider-agnostic adapter seam for future answer-generation integrations.
//! `spark-mcp` remains retrieval-only by default; no provider runtime is enabled.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Gemini,
}

impl LlmProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
        }
    }
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self::Gemini
    }
}

#[derive(Debug, Clone)]
pub struct LlmRuntime;

#[derive(Debug, Clone)]
pub struct LlmAnswerRequest {
    pub question: String,
    pub provider: LlmProvider,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LlmAnswerResult {
    pub ok: bool,
    pub provider: String,
    pub answer_markdown: String,
}

#[derive(Debug)]
pub enum LlmAnswerError {
    NotConfigured,
}

impl std::fmt::Display for LlmAnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "no llm provider is configured; spark-mcp currently runs retrieval-only"
            ),
        }
    }
}

impl std::error::Error for LlmAnswerError {}

impl LlmRuntime {
    pub fn new() -> Self {
        Self
    }

    pub async fn answer(
        &self,
        _request: LlmAnswerRequest,
    ) -> Result<LlmAnswerResult, LlmAnswerError> {
        Err(LlmAnswerError::NotConfigured)
    }
}
