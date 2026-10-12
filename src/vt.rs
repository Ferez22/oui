//! Panic isolation for the vt100 emulator.
//!
//! Pane output is untrusted, and a bug in the emulator must not take down the
//! daemon (it parses under the global state lock) or the client. `process`
//! runs the parser under `catch_unwind`; on a panic the caller resets that
//! pane's emulator and carries on.

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

thread_local! {
    /// Set while `process` runs: the panic hook records instead of printing
    /// (a message on stderr would garble the client's screen).
    static GUARDED: Cell<bool> = const { Cell::new(false) };
    static LAST: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Wraps the current panic hook once: quiet inside `process`, unchanged elsewhere.
fn install_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if GUARDED.with(Cell::get) {
                let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
                LAST.with(|l| *l.borrow_mut() = Some(format!("{}{at}", payload_text(info.payload()))));
            } else {
                prev(info);
            }
        }));
    });
}

fn payload_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// Feed `data` to `parser`. `Err` (the panic message) means the emulator
/// panicked: its state is unreliable and the caller must replace it.
pub fn process<CB: vt100::Callbacks>(parser: &mut vt100::Parser<CB>, data: &[u8]) -> Result<(), String> {
    install_hook();
    GUARDED.with(|g| g.set(true));
    let r = panic::catch_unwind(AssertUnwindSafe(|| parser.process(data)));
    GUARDED.with(|g| g.set(false));
    r.map_err(|p| LAST.with(|l| l.borrow_mut().take()).unwrap_or_else(|| payload_text(&*p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_char_in_one_column_parser_is_contained() {
        // vt100 0.16.2 panics on a width-2 char in a 1-column grid (the bug
        // oUI's 2-column minimum avoids); the wrapper must contain it.
        let mut p = vt100::Parser::new(24, 1, 0);
        let err = process(&mut p, "中".as_bytes()).expect_err("vt100 no longer panics here; keep the test honest");
        assert!(!err.is_empty());
        // Not stuck in quiet mode: a later, normal parse works.
        let mut ok = vt100::Parser::new(24, 2, 0);
        assert_eq!(process(&mut ok, "中😀\r\nab".as_bytes()), Ok(()));
        assert!(ok.screen().contents().contains('中'));
    }

    #[test]
    fn two_columns_never_panic_on_wide_chars() {
        let mut p = vt100::Parser::new(3, 2, 10);
        for input in ["中", "a中", "中中中", "\x1b[1;2H中", "\x1b[3;2H中中", "😀a😀", "a\u{301}中", "\x1b[4h中a中"] {
            assert_eq!(process(&mut p, input.as_bytes()), Ok(()), "{input:?}");
        }
        p.screen_mut().set_size(3, 2);
        assert_eq!(process(&mut p, "中\r\n中".as_bytes()), Ok(()));
    }
}
