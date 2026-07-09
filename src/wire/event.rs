//! The unified streaming event model (specification §4): both wire adapters
//! normalize onto this, so everything downstream is provider-agnostic.

/// One normalized increment of a streaming response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A fragment of the assistant's text answer.
    TextDelta(String),
    /// A fragment of a tool call's arguments (phase-2 agent loop).
    ToolCallDelta {
        index: u64,
        id: Option<String>,
        name: Option<String>,
        args_fragment: String,
    },
    /// End of the response.
    Done {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}
