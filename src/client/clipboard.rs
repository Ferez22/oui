//! Host clipboard: OSC 52 to the host terminal (works over ssh) plus the
//! platform tool when present (`pbcopy`, `wl-copy`, `xclip`).
//!
//! `OUI_CLIPBOARD_FILE` (tests): copied text is also written to that file and
//! the platform tools are never run.

use std::io::Write;
use std::process::{Command, Stdio};

/// Largest text sent with OSC 52; many terminals drop bigger payloads.
pub const OSC52_MAX_TEXT: usize = 100 * 1024;

/// Copy `text`. Returns a status-bar message.
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

/// An app in a pane set the clipboard with OSC 52 (`b64` as it sent it).
pub fn copy_from_app(b64: &[u8]) {
    if let Some(text) = base64_decode(b64) {
        copy(&text);
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
}
