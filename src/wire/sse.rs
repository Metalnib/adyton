//! Server-Sent-Events framing over a blocking line stream (specification
//! §4.1/§4.2). Implements the SSE subset LLM providers actually use, plus the
//! quirks seen in the wild:
//! - `:` comment lines (router keep-alives) are skipped;
//! - `event:` names are captured for Anthropic's named events;
//! - multiple `data:` lines per event join with `\n` per the SSE spec;
//! - a final event without a trailing blank line is still delivered
//!   (spec says discard; sloppy servers say otherwise);
//! - `id:`/`retry:`/unknown fields are ignored.
//!
//! JSON interpretation of `data` (including the openai `[DONE]` sentinel) is
//! the adapters' job — framing stays protocol-pure.

use std::io::BufRead;

use crate::error::{Error, Result};

/// One SSE event as it came off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub data: String,
}

pub struct SseFrames<R> {
    reader: R,
    finished: bool,
}

impl<R: BufRead> SseFrames<R> {
    pub fn new(reader: R) -> Self {
        SseFrames {
            reader,
            finished: false,
        }
    }
}

impl<R: BufRead> Iterator for SseFrames<R> {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut event: Option<String> = None;
        let mut data: Option<String> = None;
        loop {
            let mut raw = String::new();
            match self.reader.read_line(&mut raw) {
                Ok(0) => {
                    self.finished = true;
                    // Robustness over pedantry: deliver a pending final event.
                    return data.map(|data| Ok(Frame { event, data }));
                }
                Ok(_) => {}
                Err(err) => {
                    self.finished = true;
                    return Some(Err(Error::Provider(format!("stream read failed: {err}"))));
                }
            }
            let line = raw.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                match data.take() {
                    Some(data) => return Some(Ok(Frame { event, data })),
                    // Spec: a blank line with no data dispatches nothing and
                    // resets the event name.
                    None => event = None,
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
                (field, value.strip_prefix(' ').unwrap_or(value))
            });
            match field {
                "data" => match &mut data {
                    Some(buffer) => {
                        buffer.push('\n');
                        buffer.push_str(value);
                    }
                    None => data = Some(value.to_owned()),
                },
                "event" => event = Some(value.to_owned()),
                _ => {} // id, retry, unknown fields
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, SseFrames};
    use std::io::{BufReader, Cursor, Read};

    fn frames_of(input: &str) -> Vec<Frame> {
        SseFrames::new(Cursor::new(input.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("no io errors")
    }

    fn frame(event: Option<&str>, data: &str) -> Frame {
        Frame {
            event: event.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn framing_grammar_table() {
        #[rustfmt::skip]
        let table: &[(&str, &str, Vec<Frame>)] = &[
            ("single data frame",
             "data: {\"a\":1}\n\n",
             vec![frame(None, "{\"a\":1}")]),
            ("openai done sentinel passes through as data",
             "data: {\"a\":1}\n\ndata: [DONE]\n\n",
             vec![frame(None, "{\"a\":1}"), frame(None, "[DONE]")]),
            ("anthropic named events",
             "event: content_block_delta\ndata: {\"d\":1}\n\nevent: message_stop\ndata: {}\n\n",
             vec![frame(Some("content_block_delta"), "{\"d\":1}"), frame(Some("message_stop"), "{}")]),
            ("multi-line data joins with newline",
             "data: line one\ndata: line two\n\n",
             vec![frame(None, "line one\nline two")]),
            ("comment keep-alives are skipped",
             ": OPENROUTER PROCESSING\ndata: {\"a\":1}\n\n: ping\n\n",
             vec![frame(None, "{\"a\":1}")]),
            ("id and retry fields are ignored",
             "id: 42\nretry: 100\ndata: x\n\n",
             vec![frame(None, "x")]),
            ("crlf line endings",
             "event: ping\r\ndata: {}\r\n\r\n",
             vec![frame(Some("ping"), "{}")]),
            ("no space after colon",
             "data:tight\n\n",
             vec![frame(None, "tight")]),
            ("empty data line contributes an empty segment",
             "data:\ndata: after\n\n",
             vec![frame(None, "\nafter")]),
            ("event name without data dispatches nothing and resets",
             "event: orphan\n\ndata: next\n\n",
             vec![frame(None, "next")]),
            ("final event without trailing blank line is delivered",
             "data: {\"a\":1}\n\ndata: [DONE]",
             vec![frame(None, "{\"a\":1}"), frame(None, "[DONE]")]),
            ("blank-only stream yields nothing",
             "\n\n\n",
             vec![]),
            ("interleaved pings between deltas",
             "event: ping\ndata: {}\n\ndata: {\"d\":1}\n\nevent: ping\ndata: {}\n\ndata: {\"d\":2}\n\n",
             vec![frame(Some("ping"), "{}"), frame(None, "{\"d\":1}"), frame(Some("ping"), "{}"), frame(None, "{\"d\":2}")]),
        ];
        for (name, input, expected) in table {
            assert_eq!(&frames_of(input), expected, "case: {name}");
        }
    }

    /// Reader that yields a prefix, then fails — the mid-stream death case
    /// from the transport's perspective.
    struct FailingReader {
        data: Cursor<Vec<u8>>,
        drained: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.data.read(buf) {
                Ok(0) if !self.drained => {
                    self.drained = true;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "boom",
                    ))
                }
                Ok(0) => Ok(0),
                other => other,
            }
        }
    }

    #[test]
    fn io_error_mid_stream_surfaces_after_completed_frames() {
        let reader = BufReader::new(FailingReader {
            data: Cursor::new(b"data: {\"a\":1}\n\ndata: torn".to_vec()),
            drained: false,
        });
        let mut frames = SseFrames::new(reader);

        let first = frames.next().expect("first frame").expect("ok");
        assert_eq!(first.data, "{\"a\":1}");
        assert!(
            frames.next().expect("second item").is_err(),
            "torn frame → error"
        );
        assert!(frames.next().is_none(), "iterator ends after the error");
    }
}
