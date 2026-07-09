//! Blocking HTTP transport (specification §4.3): ureq + rustls behind a thin
//! trait, so command code tests against a fake and the phase-2 daemon can add
//! connection pooling without touching callers.
//!
//! Contract:
//! - connect timeout 5 s; whole-request timeout from config;
//! - exactly one immediate retry when the connection dies before the first
//!   response byte (idempotent at that point — nothing has streamed yet);
//! - non-2xx never reaches the SSE parser: it becomes `Error::Http` carrying
//!   the (capped) error body.

use std::io::{BufRead, BufReader, Read as _};
use std::time::Duration;

use crate::error::{Error, Result};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Provider error bodies are small JSON; the cap only guards against a
/// misbehaving endpoint streaming garbage into an error path.
const ERROR_BODY_CAP: u64 = 64 * 1024;

#[derive(Debug)]
pub struct HttpRequest {
    pub url: String,
    /// Includes the auth header; the secret lives only in process memory.
    pub headers: Vec<(String, String)>,
    /// JSON request body, already serialized by the wire adapter.
    pub body: Vec<u8>,
}

pub trait Transport {
    /// POST and hand back the response body as a buffered line stream.
    fn post_stream(&self, request: &HttpRequest) -> Result<Box<dyn BufRead>>;
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl std::fmt::Debug for UreqTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UreqTransport")
    }
}

impl UreqTransport {
    pub fn new(request_timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(request_timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            // Status handling is ours: 4xx/5xx must yield Error::Http with the
            // provider's body, not a bodyless transport error.
            .http_status_as_error(false)
            .build();
        UreqTransport {
            agent: config.into(),
        }
    }

    fn send_once(
        &self,
        request: &HttpRequest,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let mut builder = self.agent.post(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder.send(&request.body[..])
    }
}

impl Transport for UreqTransport {
    fn post_stream(&self, request: &HttpRequest) -> Result<Box<dyn BufRead>> {
        let response = match self.send_once(request) {
            Err(err) if died_before_first_byte(&err) => self.send_once(request),
            other => other,
        }
        .map_err(|err| Error::Provider(format!("request to {} failed: {err}", request.url)))?;

        let status = response.status().as_u16();
        let body = response.into_body();
        if !(200..300).contains(&status) {
            let mut text = String::new();
            let _ = body
                .into_reader()
                .take(ERROR_BODY_CAP)
                .read_to_string(&mut text);
            return Err(Error::Http {
                status,
                body: text.trim().to_owned(),
            });
        }
        Ok(Box::new(BufReader::new(body.into_reader())))
    }
}

/// Retry window: only failures where no response byte arrived, so the request
/// provably never started streaming (send/connect stage). `BrokenPipe` is the
/// EPIPE seen when the server drops the socket while we are still writing the
/// request — same pre-first-byte class, just a different race winner.
fn died_before_first_byte(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::BrokenPipe
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpRequest, Transport, UreqTransport};
    use crate::error::Error;
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    enum Behavior {
        /// Drain the request, then drop before writing anything — a
        /// deterministic death before the first response byte. (Dropping
        /// without reading would race EPIPE-on-write vs reset-on-read.)
        CloseImmediately,
        /// Serve a complete response.
        Respond(Vec<u8>),
        /// Serve a prefix, then drop mid-body — death after the first byte.
        RespondThenDie(Vec<u8>),
    }

    struct MockServer {
        addr: std::net::SocketAddr,
        hits: Arc<AtomicUsize>,
    }

    impl MockServer {
        fn start(behaviors: Vec<Behavior>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let hits_in_thread = Arc::clone(&hits);
            std::thread::spawn(move || {
                for behavior in behaviors {
                    let (mut stream, _) = listener.accept().expect("accept");
                    hits_in_thread.fetch_add(1, Ordering::SeqCst);
                    match behavior {
                        Behavior::CloseImmediately => {
                            read_request(&mut stream);
                            drop(stream);
                        }
                        Behavior::Respond(bytes) => {
                            read_request(&mut stream);
                            let _ = stream.write_all(&bytes);
                        }
                        Behavior::RespondThenDie(bytes) => {
                            read_request(&mut stream);
                            let _ = stream.write_all(&bytes);
                            let _ = stream.flush();
                            drop(stream);
                        }
                    }
                }
            });
            MockServer { addr, hits }
        }

        fn url(&self) -> String {
            format!("http://{}/v1/chat/completions", self.addr)
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    /// Drain the request head plus `Content-Length` body so the client never
    /// sees a reset while still sending.
    fn read_request(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return,
            }
        }
        let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
        let content_length = head_text
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_length];
        let _ = stream.read_exact(&mut body);
    }

    fn http_response(status_line: &str, content_type: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status_line}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn request(url: String) -> HttpRequest {
        HttpRequest {
            url,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: br#"{"stream":true}"#.to_vec(),
        }
    }

    fn transport() -> UreqTransport {
        UreqTransport::new(Duration::from_secs(10))
    }

    #[test]
    fn streams_a_chunked_body_unchunked_line_by_line() {
        // SSE-shaped payload split across chunks at an awkward boundary.
        let payload_a = "data: hel";
        let payload_b = "lo\n\ndata: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n\
             {:x}\r\n{payload_a}\r\n{:x}\r\n{payload_b}\r\n0\r\n\r\n",
            payload_a.len(),
            payload_b.len(),
        );
        let server = MockServer::start(vec![Behavior::Respond(response.into_bytes())]);

        let reader = transport()
            .post_stream(&request(server.url()))
            .expect("stream");
        let lines: Vec<String> = reader.lines().map(|l| l.expect("line")).collect();
        assert_eq!(lines, vec!["data: hello", "", "data: [DONE]", ""]);
    }

    #[test]
    fn non_2xx_becomes_http_error_with_provider_body() {
        for (status_line, status, message) in [
            (
                "401 Unauthorized",
                401_u16,
                r#"{"error":{"message":"bad api key"}}"#,
            ),
            (
                "429 Too Many Requests",
                429,
                r#"{"error":{"message":"rate limited"}}"#,
            ),
            (
                "500 Internal Server Error",
                500,
                r#"{"error":{"message":"boom"}}"#,
            ),
        ] {
            let server = MockServer::start(vec![Behavior::Respond(http_response(
                status_line,
                "application/json",
                message,
            ))]);
            match transport().post_stream(&request(server.url())) {
                Err(Error::Http { status: got, body }) => {
                    assert_eq!(got, status);
                    assert_eq!(body, message, "error body must reach the caller");
                }
                Err(other) => panic!("expected Http error, got {other:?}"),
                Ok(_) => panic!("expected Http error, got a stream"),
            }
        }
    }

    #[test]
    fn retries_exactly_once_when_connection_dies_before_first_byte() {
        let ok = http_response("200 OK", "text/event-stream", "data: [DONE]\n\n");
        let server = MockServer::start(vec![Behavior::CloseImmediately, Behavior::Respond(ok)]);

        let reader = transport()
            .post_stream(&request(server.url()))
            .expect("retried");
        assert_eq!(
            reader.lines().next().expect("line").expect("io"),
            "data: [DONE]"
        );
        assert_eq!(server.hits(), 2, "first attempt + one retry");
    }

    #[test]
    fn does_not_retry_more_than_once() {
        let server =
            MockServer::start(vec![Behavior::CloseImmediately, Behavior::CloseImmediately]);
        match transport().post_stream(&request(server.url())) {
            Err(Error::Provider(_)) => {}
            Err(other) => panic!("expected Provider error, got {other:?}"),
            Ok(_) => panic!("expected Provider error, got a stream"),
        }
        assert_eq!(server.hits(), 2, "no third attempt");
    }

    #[test]
    fn death_after_first_byte_surfaces_as_read_error_without_retry() {
        // Headers plus an unterminated chunk, then the server dies.
        let partial = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\nff\r\ndata: trunc".to_vec();
        let server = MockServer::start(vec![Behavior::RespondThenDie(partial)]);

        let reader = transport()
            .post_stream(&request(server.url()))
            .expect("headers ok");
        let result: std::result::Result<Vec<String>, std::io::Error> = reader.lines().collect();
        assert!(
            result.is_err(),
            "mid-stream death must surface to the caller"
        );
        assert_eq!(server.hits(), 1, "no retry after the first byte");
    }
}
