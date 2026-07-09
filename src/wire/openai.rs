//! openai-compatible wire adapter (specification §4.1). Reaches the openai
//! API, ollama, lm studio, vllm, llama.cpp, litellm, openrouter, nim and
//! routers via a `base_url` swap; `base_url` includes `/v1` by convention.

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

pub fn build_request(
    base_url: &str,
    api_key: Option<&Secret>,
    extra_headers: &[String],
    params: &ChatParams<'_>,
) -> HttpRequest {
    let mut body = Object::new();
    body.insert("model".to_owned(), json::str_value(params.model));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert(
        params.token_param.as_str().to_owned(),
        json::u64_value(u64::from(params.max_tokens)),
    );
    if let Some(temperature) = params.temperature {
        body.insert("temperature".to_owned(), json::f64_value(temperature));
    }
    let mut messages = Array::new();
    for (role, content) in [("system", params.system), ("user", params.user)] {
        if content.is_empty() {
            continue;
        }
        let mut message = Object::new();
        message.insert("role".to_owned(), json::str_value(role));
        message.insert("content".to_owned(), json::str_value(content));
        messages.push(Value::Object(message));
    }
    body.insert("messages".to_owned(), Value::Array(messages));

    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
    // Local runners (Ollama, llama.cpp) accept keyless requests.
    if let Some(key) = api_key {
        headers.push((
            "authorization".to_owned(),
            format!("Bearer {}", key.expose()),
        ));
    }
    headers.extend(split_extra_headers(extra_headers));

    HttpRequest {
        url: format!("{base_url}/chat/completions"),
        headers,
        body: json::to_string(&Value::Object(body)).into_bytes(),
    }
}

// --- streaming response → unified events --------------------------------

/// `chat.completion.chunk` — the superset observed across compatible servers.
/// Everything optional: role-only chunks, usage-only final chunks (empty or
/// absent `choices`), and servers that skip fields freely.
#[derive(Deserialize)]
struct Chunk {
    choices: Option<Vec<Choice>>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Option<Delta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize)]
struct WireToolCall {
    index: Option<u64>,
    id: Option<String>,
    function: Option<WireFunction>,
}

#[derive(Deserialize)]
struct WireFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

/// Some compatible servers report errors as a JSON frame mid-stream instead
/// of an HTTP status.
#[derive(Deserialize)]
struct ErrorFrame {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
}

pub struct Events<R: BufRead> {
    frames: SseFrames<R>,
    queue: VecDeque<Event>,
    finished: bool,
    done_emitted: bool,
    pending_stop: Option<String>,
    pending_usage: Option<Usage>,
}

pub fn events<R: BufRead>(reader: R) -> Events<R> {
    Events {
        frames: SseFrames::new(reader),
        queue: VecDeque::new(),
        finished: false,
        done_emitted: false,
        pending_stop: None,
        pending_usage: None,
    }
}

impl<R: BufRead> Events<R> {
    fn done(&mut self) -> Event {
        self.done_emitted = true;
        Event::Done {
            stop_reason: self.pending_stop.take(),
            usage: self.pending_usage.take(),
        }
    }

    fn ingest(&mut self, chunk: &Chunk) {
        for choice in chunk.choices.iter().flatten() {
            if let Some(delta) = &choice.delta {
                if let Some(content) = &delta.content
                    && !content.is_empty()
                {
                    self.queue.push_back(Event::TextDelta(content.clone()));
                }
                for call in delta.tool_calls.iter().flatten() {
                    let function = call.function.as_ref();
                    self.queue.push_back(Event::ToolCallDelta {
                        index: call.index.unwrap_or(0),
                        id: call.id.clone(),
                        name: function.and_then(|f| f.name.clone()),
                        args_fragment: function
                            .and_then(|f| f.arguments.clone())
                            .unwrap_or_default(),
                    });
                }
            }
            if let Some(reason) = &choice.finish_reason {
                self.pending_stop = Some(reason.clone());
            }
        }
        if let Some(usage) = &chunk.usage {
            self.pending_usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            });
        }
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
                // Robustness: a compatible server ending the stream without
                // `[DONE]` still yields a Done with whatever was gathered.
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
                Some(Ok(frame)) => {
                    if frame.data == "[DONE]" {
                        self.finished = true;
                        if !self.done_emitted {
                            return Some(Ok(self.done()));
                        }
                        return None;
                    }
                    // Error probe first: Chunk is all-optional by design, so
                    // an error frame would otherwise parse as an empty chunk
                    // and be silently swallowed.
                    if let Ok(err_frame) = json::from_line::<ErrorFrame>(&frame.data) {
                        self.finished = true;
                        let message = err_frame
                            .error
                            .message
                            .unwrap_or_else(|| frame.data.clone());
                        return Some(Err(Error::Provider(format!("provider error: {message}"))));
                    }
                    match json::from_line::<Chunk>(&frame.data) {
                        Ok(chunk) => self.ingest(&chunk),
                        Err(unparseable) => {
                            self.finished = true;
                            return Some(Err(unparseable));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_request, events};
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
            model: "gpt-4o",
            system: "You generate shell commands.",
            user: "find big files",
            max_tokens: 1024,
            token_param: TokenParam::MaxTokens,
            temperature: None,
        }
    }

    fn body_of(request: &crate::wire::http::HttpRequest) -> miniserde::json::Object {
        let body = String::from_utf8(request.body.clone()).unwrap();
        match json::from_line::<Value>(&body).unwrap() {
            Value::Object(object) => object,
            other => panic!("body must be an object, got {other:?}"),
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
    fn request_carries_url_auth_and_body_shape() {
        let key = Secret::new("sk-test".to_owned());
        let extra = vec!["HTTP-Referer: https://example.test".to_owned()];
        let request = build_request("http://localhost:11434/v1", Some(&key), &extra, &params());

        assert_eq!(request.url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(header(&request, "authorization"), Some("Bearer sk-test"));
        assert_eq!(
            header(&request, "HTTP-Referer"),
            Some("https://example.test")
        );

        let body = body_of(&request);
        assert_eq!(testutil::str_of(&body, "model").as_deref(), Some("gpt-4o"));
        assert_eq!(testutil::bool_of(&body, "stream"), Some(true));
        assert_eq!(testutil::u64_of(&body, "max_tokens"), Some(1024));
        assert!(
            !body.contains_key("temperature"),
            "None must be omitted, not null"
        );
        let Value::Array(messages) = &body["messages"] else {
            panic!("messages must be an array")
        };
        assert_eq!(messages.len(), 2, "system + user");
    }

    #[test]
    fn keyless_request_has_no_authorization_header() {
        let request = build_request("http://localhost:11434/v1", None, &[], &params());
        assert_eq!(header(&request, "authorization"), None);
    }

    #[test]
    fn token_param_switch_renames_the_cap_field() {
        let reasoning = ChatParams {
            token_param: TokenParam::MaxCompletionTokens,
            ..params()
        };
        let body = body_of(&build_request("http://x/v1", None, &[], &reasoning));
        assert!(body.contains_key("max_completion_tokens"));
        assert!(!body.contains_key("max_tokens"));
    }

    #[test]
    fn temperature_is_sent_when_set() {
        let warm = ChatParams {
            temperature: Some(0.2),
            ..params()
        };
        let body = body_of(&build_request("http://x/v1", None, &[], &warm));
        let temperature = testutil::f64_of(&body, "temperature").expect("temperature present");
        assert!((temperature - 0.2).abs() < f64::EPSILON);
    }

    fn collect(stream: &str) -> Vec<Event> {
        events(Cursor::new(stream.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("stream ok")
    }

    #[test]
    fn happy_path_stream_yields_deltas_then_done() {
        let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"find ~/Downloads\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" -size +100M\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":412,\"completion_tokens\":28,\"total_tokens\":440}}\n\n\
data: [DONE]\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::TextDelta("find ~/Downloads".to_owned()),
                Event::TextDelta(" -size +100M".to_owned()),
                Event::Done {
                    stop_reason: Some("stop".to_owned()),
                    usage: Some(Usage {
                        input_tokens: Some(412),
                        output_tokens: Some(28)
                    }),
                },
            ],
            "empty role-start content is skipped; stop reason and usage ride Done"
        );
    }

    #[test]
    fn tool_call_fragments_keep_index_id_and_name() {
        let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_sysinfo\",\"arguments\":\"{\\\"ke\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ys\\\":[]}\"}}]},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_owned()),
                    name: Some("get_sysinfo".to_owned()),
                    args_fragment: "{\"ke".to_owned(),
                },
                Event::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    args_fragment: "ys\":[]}".to_owned(),
                },
                Event::Done {
                    stop_reason: None,
                    usage: None
                },
            ]
        );
    }

    #[test]
    fn stream_ending_without_done_sentinel_still_finishes() {
        let stream = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ls\"},\"finish_reason\":\"stop\"}]}\n\n";
        assert_eq!(
            collect(stream),
            vec![
                Event::TextDelta("ls".to_owned()),
                Event::Done {
                    stop_reason: Some("stop".to_owned()),
                    usage: None
                },
            ]
        );
    }

    #[test]
    fn mid_stream_error_frame_uses_the_provider_message() {
        let stream =
            "data: {\"error\":{\"message\":\"model overloaded\",\"type\":\"server_error\"}}\n\n";
        let result: Vec<_> = events(Cursor::new(stream.as_bytes())).collect();
        assert_eq!(result.len(), 1);
        match &result[0] {
            Err(Error::Provider(msg)) => {
                assert!(msg.contains("model overloaded"), "{msg}");
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_chunk_carries_the_raw_line() {
        let stream = "data: {broken\n\n";
        let result: Vec<_> = events(Cursor::new(stream.as_bytes())).collect();
        match &result[0] {
            Err(Error::Provider(msg)) => assert_eq!(msg, "unparseable chunk: {broken"),
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn usage_only_final_chunk_with_empty_choices_is_accepted() {
        let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";
        let last = collect(stream).pop().unwrap();
        assert_eq!(
            last,
            Event::Done {
                stop_reason: Some("stop".to_owned()),
                usage: Some(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(2)
                }),
            }
        );
    }
}
