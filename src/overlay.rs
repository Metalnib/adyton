//! Stderr overlay (spec §6): an in-place status area on its own thread (D2) —
//! spinner + phase, a multi-line 💭 panel while the model reasons, then the
//! dimmed streamed command; cleared on drop. Repaints with cursor-up +
//! erase-to-end (`\x1b[J`), no alternate screen.

use std::io::{IsTerminal as _, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TICK: Duration = Duration::from_millis(80);
/// Keep the rendered tail comfortably inside one row of a typical terminal.
const TAIL_COLS: usize = 72;
// Narrower than TAIL_COLS: slack for wide chars (emoji/CJK count 1 char but 2
// columns) so a panel line never soft-wraps and desyncs the cursor-up count.
const THINK_COLS: usize = 64;
const THINK_ROWS: usize = 5;

enum Msg {
    Phase(&'static str),
    Delta(String),
    Reasoning(String),
}

/// Handle held by the request pipeline; dropping it (any exit path) clears
/// the status line and joins the render thread.
pub struct Overlay {
    tx: Option<Sender<Msg>>,
    handle: Option<JoinHandle<()>>,
    thinking: bool,
}

impl Overlay {
    /// Renders only when stderr is a tty and `--plain` is absent (spec §6);
    /// `show_thinking` gates the reasoning line.
    pub fn start(plain: bool, show_thinking: bool) -> Overlay {
        if plain || !std::io::stderr().is_terminal() {
            return Overlay {
                tx: None,
                handle: None,
                thinking: false,
            };
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || render_loop(&rx));
        Overlay {
            tx: Some(tx),
            handle: Some(handle),
            thinking: show_thinking,
        }
    }

    pub fn phase(&self, name: &'static str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Phase(name));
        }
    }

    pub fn delta(&self, text: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Delta(text.to_owned()));
        }
    }

    pub fn reasoning(&self, text: &str) {
        if self.thinking
            && let Some(tx) = &self.tx
        {
            let _ = tx.send(Msg::Reasoning(text.to_owned()));
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        drop(self.tx.take()); // disconnect → render thread clears and exits
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn render_loop(rx: &Receiver<Msg>) {
    let mut stderr = std::io::stderr().lock();
    let mut phase = "";
    let mut tail = String::new();
    let mut think = String::new();
    let mut frame = 0usize;
    let mut height = 0usize;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Msg::Phase(name)) => phase = name,
            Ok(Msg::Delta(text)) => tail.push_str(&text),
            Ok(Msg::Reasoning(text)) => think.push_str(&text),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        frame = (frame + 1) % FRAMES.len();
        let lines = frame_lines(FRAMES[frame], phase, &tail, &think);
        repaint(&mut stderr, &mut height, &lines);
    }
    clear_block(&mut stderr, height);
}

/// Cursor to the top-left of a `height`-row block whose last row it sits on.
fn move_to_top<W: Write>(out: &mut W, height: usize) {
    if height > 0 {
        let _ = write!(out, "\r");
        if height > 1 {
            let _ = write!(out, "\x1b[{}A", height - 1);
        }
    }
}

/// `\x1b[J` (erase-to-end) clears a shrinking block with no residual rows.
fn repaint<W: Write>(out: &mut W, height: &mut usize, lines: &[String]) {
    move_to_top(out, *height);
    let _ = write!(out, "\x1b[J");
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        let _ = write!(out, "{line}");
    }
    let _ = out.flush();
    *height = lines.len();
}

fn clear_block<W: Write>(out: &mut W, height: usize) {
    move_to_top(out, height);
    let _ = write!(out, "\x1b[J");
    let _ = out.flush();
}

/// The streamed command wins once it starts (one line); before that, a 💭 panel
/// of the newest reasoning; otherwise spinner + phase.
fn frame_lines(spinner: char, phase: &str, tail: &str, think: &str) -> Vec<String> {
    if !tail.is_empty() {
        vec![format!(
            "{spinner} \x1b[2m{}\x1b[0m",
            last_cols(tail, TAIL_COLS)
        )]
    } else if think.is_empty() {
        vec![format!("{spinner} {phase}…")]
    } else {
        let mut lines = vec![format!("{spinner} 💭 thinking…")];
        for line in wrap_tail(think, THINK_COLS, THINK_ROWS) {
            lines.push(format!("\x1b[2m{line}\x1b[0m"));
        }
        lines
    }
}

/// Word-wrap to `width` chars/line (hard-breaking over-long tokens) and keep the
/// last `height` lines — a moving window over the newest reasoning.
fn wrap_tail(text: &str, width: usize, height: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in text.split_whitespace() {
        let mut chars: Vec<char> = word.chars().collect();
        while chars.len() > width {
            if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            lines.push(chars[..width].iter().collect());
            chars.drain(..width);
        }
        let wlen = chars.len();
        if wlen == 0 {
            continue;
        }
        if cur_len > 0 && cur_len + 1 + wlen > width {
            lines.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push(' ');
            cur_len += 1;
        }
        cur.extend(chars.iter());
        cur_len += wlen;
    }
    if cur_len > 0 {
        lines.push(cur);
    }
    let start = lines.len().saturating_sub(height);
    lines.split_off(start)
}

/// Last `cols` characters on one line (newlines collapse to `⏎`).
fn last_cols(text: &str, cols: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { '⏎' } else { c })
        .collect();
    let count = flat.chars().count();
    if count <= cols {
        flat
    } else {
        flat.chars().skip(count - cols).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Overlay, clear_block, frame_lines, last_cols, repaint, wrap_tail};

    #[test]
    fn spinner_and_phase_before_the_first_delta() {
        assert_eq!(frame_lines('⠋', "request", "", ""), vec!["⠋ request…"]);
    }

    #[test]
    fn command_renders_one_dimmed_line() {
        assert_eq!(
            frame_lines('⠙', "streaming", "ls -la", ""),
            vec!["⠙ \x1b[2mls -la\x1b[0m"]
        );
    }

    #[test]
    fn thinking_is_a_header_plus_panel_until_the_command_takes_over() {
        let out = frame_lines('⠹', "streaming", "", "let me check the disk");
        assert_eq!(out[0], "⠹ 💭 thinking…");
        assert_eq!(out[1], "\x1b[2mlet me check the disk\x1b[0m");
        assert_eq!(out.len(), 2);
        assert_eq!(
            frame_lines('⠸', "streaming", "df -h", "let me check the disk"),
            vec!["⠸ \x1b[2mdf -h\x1b[0m"],
            "the command collapses the panel once it starts"
        );
    }

    #[test]
    fn wrap_tail_packs_hard_breaks_and_keeps_the_last_lines() {
        assert_eq!(
            wrap_tail("hello world foo", 5, 3),
            vec!["hello", "world", "foo"]
        );
        assert_eq!(wrap_tail("aa bb cc dd", 2, 2), vec!["cc", "dd"]);
        assert_eq!(wrap_tail("abcdefgh", 3, 5), vec!["abc", "def", "gh"]);
        for line in wrap_tail("supercalifragilistic and then some more words", 6, 3) {
            assert!(line.chars().count() <= 6, "no line exceeds width: {line:?}");
        }
    }

    #[test]
    fn repaint_grows_shrinks_and_drop_clears_without_residue() {
        let mut buf: Vec<u8> = Vec::new();
        let mut h = 0;
        repaint(
            &mut buf,
            &mut h,
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
        );
        assert_eq!(h, 3);
        assert_eq!(String::from_utf8(buf.clone()).unwrap(), "\x1b[Ja\nb\nc");

        buf.clear();
        repaint(&mut buf, &mut h, &["x".to_owned()]);
        assert_eq!(h, 1);
        assert_eq!(String::from_utf8(buf.clone()).unwrap(), "\r\x1b[2A\x1b[Jx");

        buf.clear();
        clear_block(&mut buf, h);
        assert_eq!(String::from_utf8(buf).unwrap(), "\r\x1b[J");
    }

    #[test]
    fn tail_is_single_line_and_capped() {
        assert_eq!(last_cols("a\nb", 10), "a⏎b");
        let long: String = "x".repeat(100);
        assert_eq!(last_cols(&long, 8).chars().count(), 8);
    }

    #[test]
    fn disabled_overlay_is_inert() {
        // stderr is not a tty under cargo test, so start() returns the inert handle.
        let overlay = Overlay::start(false, true);
        overlay.phase("context");
        overlay.delta("ls");
        overlay.reasoning("hmm");
        drop(overlay);
        let plain = Overlay::start(true, false);
        plain.phase("x");
        drop(plain);
    }
}
