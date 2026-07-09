//! Provider wire layer (architecture D4): transport (S3), the SSE reader and
//! unified event model (S4), and the two adapters (S5 openai, S6 anthropic).
//! All JSON stays inside this module tree (architecture D3).

pub mod anthropic;
pub mod event;
pub mod http;
pub mod json;
pub mod openai;
pub mod sse;

use std::io::BufRead;

use crate::config::{TokenParam, WireKind};
use crate::error::Result;
use crate::secret::Secret;

/// Adapter dispatch: one call site in the pipeline, the §4 wire difference
/// stays inside this module tree.
pub fn build_request(
    wire: WireKind,
    base_url: &str,
    api_key: Option<&Secret>,
    extra_headers: &[String],
    params: &ChatParams<'_>,
) -> http::HttpRequest {
    match wire {
        WireKind::Openai => openai::build_request(base_url, api_key, extra_headers, params),
        WireKind::Anthropic => anthropic::build_request(base_url, api_key, extra_headers, params),
    }
}

pub fn events(
    wire: WireKind,
    reader: Box<dyn BufRead>,
) -> Box<dyn Iterator<Item = Result<event::Event>>> {
    match wire {
        WireKind::Openai => Box::new(openai::events(reader)),
        WireKind::Anthropic => Box::new(anthropic::events(reader)),
    }
}

/// Provider-agnostic description of one chat request (MVP: system + one user
/// turn; the phase-2 agent loop generalizes this to a message list).
#[derive(Debug, Clone, Copy)]
pub struct ChatParams<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    pub max_tokens: u32,
    /// Only the openai wire uses this; anthropic always sends `max_tokens`.
    pub token_param: TokenParam,
    pub temperature: Option<f64>,
}

/// Profile `extra_headers` entries are validated as `Name: value` at config
/// parse time; this splits them for the request builders.
fn split_extra_headers(raw: &[String]) -> impl Iterator<Item = (String, String)> + '_ {
    raw.iter().filter_map(|header| {
        header
            .split_once(':')
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
    })
}
