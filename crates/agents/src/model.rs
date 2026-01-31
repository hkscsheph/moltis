use std::pin::Pin;

use {async_trait::async_trait, tokio_stream::Stream};

/// Events emitted during streaming LLM completion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    Done(Usage),
    Error(String),
}

/// LLM provider trait (Anthropic, OpenAI, Google, etc.).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Model identifier (e.g. "claude-sonnet-4-20250514", "gpt-4o").
    fn id(&self) -> &str;

    async fn complete(
        &self,
        messages: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> anyhow::Result<CompletionResponse>;

    /// Whether this provider supports tool/function calling.
    /// Defaults to false; providers that handle the `tools` parameter
    /// in `complete()` should override this to return true.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Context window size in tokens for this model.
    /// Used to detect when conversation approaches the limit and trigger auto-compact.
    fn context_window(&self) -> u32 {
        200_000
    }

    /// Stream a completion, yielding delta/done/error events.
    fn stream(
        &self,
        messages: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>>;
}

/// Response from an LLM completion call.
#[derive(Debug)]
pub struct CompletionResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}
