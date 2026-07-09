//! anthropic Messages wire adapter (specification §4.2). Native rather than
//! via the openai-compat shim because the shim is documented lossy (no prompt
//! caching, structured outputs, extended thinking). `base_url` excludes the
//! version prefix by convention; the path here is `/v1/messages`.

use std::collections::VecDeque;
use std::io::BufRead;

use miniserde::Deserialize;
use miniserde::json::{Array, Object, Value};

use crate::error::{Error, Result};
use crate::secret::Secret;
use crate::wire::event::{Event, Usage};
use crate::wire::http::HttpRequest;
use crate::wire::sse::SseFrames;
use crate::wire::{ChatParams, json, split_extra_headers};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub fn build_request(
    base_url: &str,
    api_key: Option<&Secret>,
    extra_headers: &[String],
    params: &ChatParams<'_>,
) -> HttpRequest {
    let mut body = Object::new();
    body.insert("model".to_owned(), json::str_value(params.model));
    body.insert("stream".to_owned(), Value::Bool(true));
    // Always `max_tokens` (required by the API); token_param is an openai knob.
    body.insert(
        "max_tokens".to_owned(),
        json::u64_value(u64::from(params.max_tokens)),
    );
    if !params.system.is_empty() {
        // Top-level `system`, not a message — a defining wire difference.
        body.insert("system".to_owned(), json::str_value(params.system));
    }
    if let Some(temperature) = params.temperature {
        body.insert("temperature".to_owned(), json::f64_value(temperature));
    }
    let mut message = Object::new();
    message.insert("role".to_owned(), json::str_value("user"));
    message.insert("content".to_owned(), json::str_value(params.user));
    let mut messages = Array::new();
    messages.push(Value::Object(message));
    body.insert("messages".to_owned(), Value::Array(messages));
    json::merge_into(&mut body, params.extra_body);

    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
    ];
    if let Some(key) = api_key {
        // x-api-key, never Bearer.
        headers.push(("x-api-key".to_owned(), key.expose().to_owned()));
    }
    headers.extend(split_extra_headers(extra_headers));

    HttpRequest {
        url: format!("{base_url}/v1/messages"),
        headers,
        body: json::to_string(&Value::Object(body)).into_bytes(),
    }
}

/// Superset of every named event; all-optional fields instead of enums
/// (miniserde has no data enums — and this is more forgiving on the wire).
#[derive(Deserialize)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: String,
    index: Option<u64>,
    message: Option<WireMessage>,
    content_block: Option<WireBlock>,
    delta: Option<WireDelta>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct WireMessage {
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireBlock {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct WireDelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

/// `content_block_start` metadata carried into that block's later deltas.
struct OpenBlock {
    index: u64,
    id: Option<String>,
    name: Option<String>,
}

pub struct Events<R: BufRead> {
    frames: SseFrames<R>,
    queue: VecDeque<Event>,
    finished: bool,
    done_emitted: bool,
    open_block: Option<OpenBlock>,
    pending_stop: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

pub fn events<R: BufRead>(reader: R) -> Events<R> {
    Events {
        frames: SseFrames::new(reader),
        queue: VecDeque::new(),
        finished: false,
        done_emitted: false,
        open_block: None,
        pending_stop: None,
        input_tokens: None,
        output_tokens: None,
    }
}

impl<R: BufRead> Events<R> {
    fn done(&mut self) -> Event {
        self.done_emitted = true;
        let usage =
            (self.input_tokens.is_some() || self.output_tokens.is_some()).then_some(Usage {
                input_tokens: self.input_tokens.take(),
                output_tokens: self.output_tokens.take(),
            });
        Event::Done {
            stop_reason: self.pending_stop.take(),
            usage,
        }
    }

    /// Dispatch on the JSON `type`; the SSE `event:` name mirrors it and is
    /// deliberately ignored (single source of truth). `Ok(true)` ends the stream.
    fn ingest(&mut self, wire: WireEvent) -> Result<bool> {
        match wire.kind.as_str() {
            "message_start" => {
                if let Some(usage) = wire.message.and_then(|m| m.usage) {
                    self.input_tokens = usage.input_tokens.or(self.input_tokens);
                    self.output_tokens = usage.output_tokens.or(self.output_tokens);
                }
            }
            "content_block_start" => {
                self.open_block = wire.content_block.map(|block| OpenBlock {
                    index: wire.index.unwrap_or(0),
                    // Only tool_use blocks carry id/name; text blocks give None.
                    id: block.id.filter(|_| block.kind == "tool_use"),
                    name: block.name,
                });
            }
            "content_block_delta" => {
                let index = wire.index.unwrap_or(0);
                if let Some(delta) = wire.delta {
                    match delta.kind.as_deref() {
                        Some("text_delta") => {
                            if let Some(text) = delta.text
                                && !text.is_empty()
                            {
                                self.queue.push_back(Event::TextDelta(text));
                            }
                        }
                        Some("input_json_delta") => {
                            let block = self.open_block.as_ref().filter(|b| b.index == index);
                            self.queue.push_back(Event::ToolCallDelta {
                                index,
                                id: block.and_then(|b| b.id.clone()),
                                name: block.and_then(|b| b.name.clone()),
                                args_fragment: delta.partial_json.unwrap_or_default(),
                            });
                        }
                        // Only streamed when the request opts into thinking; ready anyway.
                        Some("thinking_delta") => {
                            if let Some(thinking) = delta.thinking
                                && !thinking.is_empty()
                            {
                                self.queue.push_back(Event::ReasoningDelta(thinking));
                            }
                        }
                        // signature_delta and future kinds: skip.
                        _ => {}
                    }
                }
            }
            "content_block_stop" => self.open_block = None,
            "message_delta" => {
                if let Some(delta) = wire.delta
                    && let Some(stop) = delta.stop_reason
                {
                    self.pending_stop = Some(stop);
                }
                if let Some(usage) = wire.usage {
                    self.input_tokens = usage.input_tokens.or(self.input_tokens);
                    self.output_tokens = usage.output_tokens.or(self.output_tokens);
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                let error = wire.error.unwrap_or(WireError {
                    kind: None,
                    message: None,
                });
                return Err(Error::Provider(format!(
                    "provider error: {}: {}",
                    error.kind.as_deref().unwrap_or("error"),
                    error.message.as_deref().unwrap_or("(no message)"),
                )));
            }
            // ping and unknown future events: skip.
            _ => {}
        }
        Ok(false)
    }
}

impl<R: BufRead> Iterator for Events<R> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            match self.frames.next() {
                // Robustness: EOF without message_stop still yields a Done.
                None => {
                    self.finished = true;
                    if !self.done_emitted {
                        return Some(Ok(self.done()));
                    }
                    return None;
                }
                Some(Err(err)) => {
                    self.finished = true;
                    return Some(Err(err));
                }
                Some(Ok(frame)) => match json::from_line::<WireEvent>(&frame.data) {
                    Ok(wire) => match self.ingest(wire) {
                        Ok(false) => {}
                        Ok(true) => {
                            self.finished = true;
                            if !self.done_emitted {
                                return Some(Ok(self.done()));
                            }
                            return None;
                        }
                        Err(err) => {
                            self.finished = true;
                            return Some(Err(err));
                        }
                    },
                    Err(unparseable) => {
                        self.finished = true;
                        return Some(Err(unparseable));
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ANTHROPIC_VERSION, build_request, events};
    use crate::config::TokenParam;
    use crate::error::Error;
    use crate::secret::Secret;
    use crate::wire::ChatParams;
    use crate::wire::event::{Event, Usage};
    use crate::wire::json;
    use crate::wire::json::testutil;
    use miniserde::json::Value;
    use std::io::Cursor;

    fn params() -> ChatParams<'static> {
        ChatParams {
            model: "claude-sonnet-4-20250514",
            system: "You generate shell commands.",
            user: "find big files",
            max_tokens: 1024,
            token_param: TokenParam::MaxTokens,
            temperature: None,
            extra_body: None,
        }
    }

    fn header<'a>(request: &'a crate::wire::http::HttpRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn request_uses_native_headers_and_top_level_system() {
        let key = Secret::new("sk-ant-test".to_owned());
        let request = build_request("https://api.anthropic.com", Some(&key), &[], &params());

        assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(header(&request, "x-api-key"), Some("sk-ant-test"));
        assert_eq!(
            header(&request, "anthropic-version"),
            Some(ANTHROPIC_VERSION)
        );
        assert_eq!(header(&request, "authorization"), None, "never Bearer");

        let body = String::from_utf8(request.body.clone()).unwrap();
        let Value::Object(body) = json::from_line::<Value>(&body).unwrap() else {
            panic!("body must be an object")
        };
        assert_eq!(
            testutil::str_of(&body, "system").as_deref(),
            Some("You generate shell commands."),
            "system is top-level, not a message"
        );
        assert_eq!(
            testutil::u64_of(&body, "max_tokens"),
            Some(1024),
            "always required"
        );
        let Value::Array(messages) = &body["messages"] else {
            panic!("messages must be an array")
        };
        assert_eq!(messages.len(), 1, "user turn only — no system message");
    }

    fn collect(stream: &str) -> Vec<Event> {
        events(Cursor::new(stream.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("stream ok")
    }

    #[test]
    fn happy_path_named_event_sequence() {
        let stream = "\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[],\"usage\":{\"input_tokens\":472,\"output_tokens\":2}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: ping\ndata: {\"type\":\"ping\"}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"rsync -avz\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" ~/src/ nas:/vol/\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":31}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::TextDelta("rsync -avz".to_owned()),
                Event::TextDelta(" ~/src/ nas:/vol/".to_owned()),
                Event::Done {
                    stop_reason: Some("end_turn".to_owned()),
                    usage: Some(Usage {
                        input_tokens: Some(472),
                        output_tokens: Some(31)
                    }),
                },
            ],
            "ping skipped; usage merged from message_start and message_delta"
        );
    }

    #[test]
    fn tool_use_block_attaches_id_and_name_to_fragments() {
        let stream = "\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_sysinfo\",\"input\":{}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"keys\\\"\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":[]}\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::ToolCallDelta {
                    index: 1,
                    id: Some("toolu_1".to_owned()),
                    name: Some("get_sysinfo".to_owned()),
                    args_fragment: "{\"keys\"".to_owned(),
                },
                Event::ToolCallDelta {
                    index: 1,
                    id: Some("toolu_1".to_owned()),
                    name: Some("get_sysinfo".to_owned()),
                    args_fragment: ":[]}".to_owned(),
                },
                Event::Done {
                    stop_reason: None,
                    usage: None
                },
            ]
        );
    }

    #[test]
    fn error_event_surfaces_type_and_message() {
        let stream = "\
event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let result: Vec<_> = events(Cursor::new(stream.as_bytes())).collect();
        assert_eq!(result.len(), 1);
        match &result[0] {
            Err(Error::Provider(msg)) => {
                assert!(
                    msg.contains("overloaded_error") && msg.contains("Overloaded"),
                    "{msg}"
                );
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn eof_without_message_stop_still_finishes() {
        let stream = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ls\"}}\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::TextDelta("ls".to_owned()),
                Event::Done {
                    stop_reason: None,
                    usage: None
                },
            ]
        );
    }

    #[test]
    fn thinking_deltas_surface_as_reasoning_and_unknown_kinds_skip() {
        let stream = "\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n\
data: {\"type\":\"some_future_event\"}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::ReasoningDelta("hmm".to_owned()),
                Event::TextDelta("ok".to_owned()),
                Event::Done {
                    stop_reason: None,
                    usage: None
                },
            ]
        );
    }

    #[test]
    fn unparseable_event_carries_the_raw_line() {
        let result: Vec<_> = events(Cursor::new(b"data: {torn".as_slice())).collect();
        match &result[0] {
            Err(Error::Provider(msg)) => assert_eq!(msg, "unparseable chunk: {torn"),
            other => panic!("expected provider error, got {other:?}"),
        }
    }
}
