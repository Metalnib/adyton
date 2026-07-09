use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Instant;

// ===== OpenAI chat.completion.chunk — full realistic model =====
#[derive(Deserialize)]
struct Chunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    system_fingerprint: Option<String>,
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    index: u64,
    delta: Delta,
    logprobs: Option<Value>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: u64,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

// ===== Anthropic streaming events — full superset model =====
#[derive(Deserialize)]
struct AEvent {
    #[serde(rename = "type")]
    kind: String,
    index: Option<u64>,
    message: Option<AMessage>,
    content_block: Option<ABlock>,
    delta: Option<ADelta>,
    usage: Option<AUsage>,
    error: Option<AError>,
}

#[derive(Deserialize)]
struct AMessage {
    id: String,
    role: String,
    model: String,
    stop_reason: Option<String>,
    usage: Option<AUsage>,
}

#[derive(Deserialize)]
struct ABlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ADelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
}

#[derive(Deserialize)]
struct AUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct AError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

// ===== request serialization — full realistic model =====
#[derive(Serialize)]
struct ChatRequest {
    model: String,
    stream: bool,
    max_tokens: u64,
    temperature: f64,
    messages: Vec<Msg>,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct Msg {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct Tool {
    #[serde(rename = "type")]
    kind: String,
    function: ToolFn,
}

#[derive(Serialize)]
struct ToolFn {
    name: String,
    description: String,
    parameters: Value,
}

const OPENAI_SAMPLES: &[&str] = &[
    r#"{"id":"chatcmpl-9x2","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-2024-08-06","system_fingerprint":"fp_44709d6fcb","choices":[{"index":0,"delta":{"role":"assistant","content":""},"logprobs":null,"finish_reason":null}]}"#,
    r#"{"id":"chatcmpl-9x2","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-2024-08-06","system_fingerprint":"fp_44709d6fcb","choices":[{"index":0,"delta":{"content":"find ~/Downloads -name '*.pdf' -mtime -7"},"logprobs":null,"finish_reason":null}]}"#,
    r#"{"id":"chatcmpl-9x2","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-2024-08-06","system_fingerprint":"fp_44709d6fcb","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"get_sysinfo","arguments":"{\"ke"}}]},"logprobs":null,"finish_reason":null}]}"#,
    r#"{"id":"chatcmpl-9x2","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-2024-08-06","system_fingerprint":"fp_44709d6fcb","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ys\":[\"os\",\"arch\"]}"}}]},"logprobs":null,"finish_reason":null}]}"#,
    r#"{"id":"chatcmpl-9x2","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-2024-08-06","system_fingerprint":"fp_44709d6fcb","choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}],"usage":{"prompt_tokens":412,"completion_tokens":28,"total_tokens":440}}"#,
];

const ANTHROPIC_SAMPLES: &[&str] = &[
    r#"{"type":"message_start","message":{"id":"msg_014p7gG3wDgGV9EUtLvnow3U","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":472,"output_tokens":2}}}"#,
    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"rsync -avz --progress ~/src/ backup@nas:/vol/src/"}}"#,
    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"uname"}}"#,
    r#"{"type":"content_block_stop","index":0}"#,
    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":31}}"#,
    r#"{"type":"message_stop"}"#,
];

fn make_request() -> ChatRequest {
    ChatRequest {
        model: "gpt-4o".into(),
        stream: true,
        max_tokens: 1024,
        temperature: 0.2,
        messages: vec![
            Msg { role: "system".into(), content: "You are a shell command generator. Output only the command, no prose. Target: macOS 15.5 arm64, zsh 5.9.".into() },
            Msg { role: "user".into(), content: "find all pdfs modified this week in downloads".into() },
        ],
        tools: vec![Tool {
            kind: "function".into(),
            function: ToolFn {
                name: "get_sysinfo".into(),
                description: "Fetch cached system information".into(),
                parameters: serde_json::from_str(r#"{"type":"object","properties":{"keys":{"type":"array","items":{"type":"string"}}},"required":["keys"]}"#).unwrap(),
            },
        }],
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();

    if mode == "--sse" {
        use std::io::Read;
        let url = std::env::args().nth(2).expect("url");
        let start = Instant::now();
        let mut resp = ureq::get(&url).call().expect("request");
        let mut r = resp.body_mut().as_reader();
        let mut buf = [0u8; 256];
        let n = r.read(&mut buf).expect("first read");
        let first = start.elapsed();
        let mut total = n;
        loop {
            let n = r.read(&mut buf).expect("read");
            if n == 0 { break; }
            total += n;
        }
        println!("sse: first {} bytes at {:.0?}, total {} bytes at {:.0?}", n, first, total, start.elapsed());
        return;
    }

    let mut content_len = 0usize;
    for s in OPENAI_SAMPLES {
        let c: Chunk = serde_json::from_str(s).expect("openai");
        content_len += c.choices[0].delta.content.as_deref().map_or(0, str::len);
    }
    for s in ANTHROPIC_SAMPLES {
        let e: AEvent = serde_json::from_str(s).expect("anthropic");
        content_len += e.delta.as_ref().and_then(|d| d.text.as_deref()).map_or(0, str::len);
    }
    println!("correctness: all {} samples parsed, content bytes {}", OPENAI_SAMPLES.len() + ANTHROPIC_SAMPLES.len(), content_len);

    const N: usize = 200_000;
    let mut bytes = 0usize;
    let mut sink = 0usize;
    let t = Instant::now();
    for i in 0..N {
        let s = OPENAI_SAMPLES[i % OPENAI_SAMPLES.len()];
        bytes += s.len();
        let c: Chunk = serde_json::from_str(s).unwrap();
        sink += c.choices[0].delta.content.as_deref().map_or(0, str::len) + c.created as usize % 7;
    }
    let dt = t.elapsed();
    println!("openai parse: {:.2} µs/op, {:.0} MB/s (sink {})", dt.as_micros() as f64 / N as f64, bytes as f64 / dt.as_secs_f64() / 1e6, sink % 1000);

    let mut bytes = 0usize;
    let mut sink = 0usize;
    let t = Instant::now();
    for i in 0..N {
        let s = ANTHROPIC_SAMPLES[i % ANTHROPIC_SAMPLES.len()];
        bytes += s.len();
        let e: AEvent = serde_json::from_str(s).unwrap();
        sink += e.kind.len() + e.index.unwrap_or(0) as usize;
    }
    let dt = t.elapsed();
    println!("anthropic parse: {:.2} µs/op, {:.0} MB/s (sink {})", dt.as_micros() as f64 / N as f64, bytes as f64 / dt.as_secs_f64() / 1e6, sink % 1000);

    const M: usize = 100_000;
    let req = make_request();
    let mut out_bytes = 0usize;
    let t = Instant::now();
    for _ in 0..M {
        out_bytes += serde_json::to_string(&req).unwrap().len();
    }
    let dt = t.elapsed();
    println!("serialize: {:.2} µs/op, {:.0} MB/s", dt.as_micros() as f64 / M as f64, out_bytes as f64 / dt.as_secs_f64() / 1e6);
}
