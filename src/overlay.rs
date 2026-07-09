//! Stderr overlay (spec §6): one status line rewritten in place — spinner +
//! phase, then the dimmed streamed command — cleared on drop. Runs on its own
//! thread (D2) so the main thread can block on the response stream. Rendering
//! uses only `\r` and `\x1b[K`; no cursor addressing, no alternate screen.

use std::io::{IsTerminal as _, Write as _};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TICK: Duration = Duration::from_millis(80);
/// Keep the rendered tail comfortably inside one row of a typical terminal.
const TAIL_COLS: usize = 72;

enum Msg {
    Phase(&'static str),
    Delta(String),
}

/// Handle held by the request pipeline; dropping it (any exit path) clears
/// the status line and joins the render thread.
pub struct Overlay {
    tx: Option<Sender<Msg>>,
    handle: Option<JoinHandle<()>>,
}

impl Overlay {
    /// Renders only when stderr is a tty and `--plain` is absent (spec §6).
    pub fn start(plain: bool) -> Overlay {
        if plain || !std::io::stderr().is_terminal() {
            return Overlay {
                tx: None,
                handle: None,
            };
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || render_loop(&rx));
        Overlay {
            tx: Some(tx),
            handle: Some(handle),
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
    let mut frame = 0usize;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Msg::Phase(name)) => phase = name,
            Ok(Msg::Delta(text)) => tail.push_str(&text),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        frame = (frame + 1) % FRAMES.len();
        let _ = write!(
            stderr,
            "\r\x1b[K{}",
            frame_line(FRAMES[frame], phase, &tail)
        );
        let _ = stderr.flush();
    }
    let _ = write!(stderr, "\r\x1b[K");
    let _ = stderr.flush();
}

/// Pure renderer: spinner + phase until the first delta, then the dimmed
/// tail of the streamed command.
fn frame_line(spinner: char, phase: &str, tail: &str) -> String {
    if tail.is_empty() {
        format!("{spinner} {phase}…")
    } else {
        format!("{spinner} \x1b[2m{}\x1b[0m", last_cols(tail, TAIL_COLS))
    }
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
    use super::{Overlay, frame_line, last_cols};

    #[test]
    fn spinner_and_phase_before_the_first_delta() {
        assert_eq!(frame_line('⠋', "request", ""), "⠋ request…");
    }

    #[test]
    fn deltas_render_dimmed_with_reset() {
        assert_eq!(
            frame_line('⠙', "streaming", "ls -la"),
            "⠙ \x1b[2mls -la\x1b[0m"
        );
    }

    #[test]
    fn tail_is_single_line_and_capped() {
        assert_eq!(last_cols("a\nb", 10), "a⏎b");
        let long: String = "x".repeat(100);
        assert_eq!(last_cols(&long, 8).chars().count(), 8);
    }

    #[test]
    fn disabled_overlay_is_inert() {
        // Non-tty stderr in tests → start() must yield a no-op handle whose
        // methods and drop never write anywhere.
        let overlay = Overlay::start(false);
        overlay.phase("context");
        overlay.delta("ls");
        drop(overlay);
        let plain = Overlay::start(true);
        plain.phase("x");
        drop(plain);
    }
}
