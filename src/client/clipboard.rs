//! Host clipboard.
//!
//! The user's own copies (copy mode, mouse selection) go out as OSC 52 to the
//! host terminal (works over ssh) and through the platform tool when present
//! (`pbcopy`, `wl-copy`, `xclip`).
//!
//! Writes from programs in panes (OSC 52 in pane output) follow
//! `clipboard.apps`: by default they are only passed on as OSC 52, so the host
//! terminal's own clipboard permission decides; the platform tool is used only
//! with `apps = "system"`. The app checks they come from the focused pane of
//! the active tab and coalesces bursts (`AppClips`).
//!
//! `OUI_CLIPBOARD_FILE` (tests): stands in for the platform tool: text that
//! would go to it is written to that file instead, and the tools never run.

use crate::config::AppClipboard;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Largest text sent with OSC 52; many terminals drop bigger payloads.
pub const OSC52_MAX_TEXT: usize = 100 * 1024;
/// Largest OSC 52 payload (base64) taken from a pane (~100 KB of text).
pub const OSC52_MAX_B64: usize = 140 * 1024;
/// At most one app clipboard write per this interval; the last one wins.
pub const APP_INTERVAL: Duration = Duration::from_millis(100);

/// Copy `text` for the user. Returns a status-bar message.
pub fn copy(text: &[u8]) -> String {
    let osc = text.len() <= OSC52_MAX_TEXT;
    if osc {
        write_osc52(&base64_encode(text));
    }
    let tool = match std::env::var_os("OUI_CLIPBOARD_FILE") {
        Some(path) => std::fs::write(path, text).is_ok(),
        None => system_copy(text),
    };
    let n = String::from_utf8_lossy(text).chars().count();
    match (osc, tool) {
        (false, false) => format!("copy: {n} chars too large for OSC 52 and no clipboard tool found"),
        _ => format!("copied {n} char{}", if n == 1 { "" } else { "s" }),
    }
}

/// A program in the focused pane set the clipboard (decoded text).
pub fn copy_from_app(text: &[u8], mode: AppClipboard) {
    match mode {
        AppClipboard::Off => {}
        AppClipboard::Osc52 if text.len() <= OSC52_MAX_TEXT => write_osc52(&base64_encode(text)),
        AppClipboard::Osc52 => {}
        AppClipboard::System => {
            copy(text);
        }
    }
}

/// vt100 callbacks for the client's pane parsers: collects OSC 52 writes
/// (base64 as sent). Clipboard reads (`?`) are never answered.
#[derive(Default)]
pub struct Catch(pub Vec<Vec<u8>>);

impl vt100::Callbacks for Catch {
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        if data != b"?" && data.len() <= OSC52_MAX_B64 {
            self.0.push(data.to_vec());
        }
    }
}

/// Rate limit for app clipboard writes: the first goes out at once, later
/// ones at most every `APP_INTERVAL`, keeping only the newest.
#[derive(Default)]
pub struct AppClips {
    last: Option<Instant>,
    pending: Option<(Vec<u8>, Instant)>,
}

impl AppClips {
    /// A new write at `now`: returns the text to copy right away, if any.
    pub fn offer(&mut self, text: Vec<u8>, now: Instant) -> Option<Vec<u8>> {
        if let Some((_, due)) = self.pending {
            self.pending = Some((text, due));
            return None;
        }
        match self.last {
            Some(t) if now < t + APP_INTERVAL => {
                self.pending = Some((text, t + APP_INTERVAL));
                None
            }
            _ => {
                self.last = Some(now);
                Some(text)
            }
        }
    }

    /// When the held-back write is due.
    pub fn due(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, t)| *t)
    }

    /// The held-back write, if it is due at `now`.
    pub fn take_due(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.due().is_some_and(|t| t <= now) {
            self.last = Some(now);
            return self.pending.take().map(|(text, _)| text);
        }
        None
    }
}

fn write_osc52(b64: &str) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();
}

/// Pipe into the first platform clipboard tool found (in the background).
fn system_copy(text: &[u8]) -> bool {
    let tools: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    } else {
        &[("xclip", &["-selection", "clipboard"]), ("wl-copy", &[])]
    };
    for (cmd, args) in tools {
        let child = Command::new(cmd).args(*args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Ok(mut child) = child {
            let text = text.to_vec();
            std::thread::spawn(move || {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&text);
                }
                let _ = child.wait();
            });
            return true;
        }
    }
    false
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn base64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for (plain, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64_encode(plain.as_bytes()), enc);
            assert_eq!(base64_decode(enc.as_bytes()).unwrap(), plain.as_bytes());
        }
        let bin: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(base64_encode(&bin).as_bytes()).unwrap(), bin);
        assert!(base64_decode(b"!!").is_none());
    }

    #[test]
    fn app_clips_are_rate_limited() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = AppClips::default();
        assert_eq!(c.offer(b"a".to_vec(), t0), Some(b"a".to_vec()));
        // A burst inside the interval: held back, the newest wins.
        assert_eq!(c.offer(b"b".to_vec(), ms(10)), None);
        assert_eq!(c.offer(b"c".to_vec(), ms(20)), None);
        assert_eq!(c.due(), Some(ms(100)));
        assert_eq!(c.take_due(ms(50)), None);
        assert_eq!(c.take_due(ms(100)), Some(b"c".to_vec()));
        assert_eq!(c.due(), None);
        // Next one within the interval of that flush waits again.
        assert_eq!(c.offer(b"d".to_vec(), ms(150)), None);
        assert_eq!(c.take_due(ms(200)), Some(b"d".to_vec()));
        // Quiet for a while: immediate.
        assert_eq!(c.offer(b"e".to_vec(), ms(1000)), Some(b"e".to_vec()));
    }

    #[test]
    fn catch_collects_osc52_writes_but_not_reads() {
        let mut p = vt100::Parser::new_with_callbacks(5, 20, 0, Catch::default());
        p.process(b"x\x1b]52;c;QVBQ\x07y\x1b]52;c;?\x07");
        // Split across two outputs.
        p.process(b"\x1b]52;c;Wl");
        p.process(b"o=\x1b\\");
        assert_eq!(p.callbacks().0, [b"QVBQ".to_vec(), b"Wlo=".to_vec()]);
    }
}
