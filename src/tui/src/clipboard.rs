//! Explicit Windows clipboard reads for form paste shortcuts only.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn paste_key(key: KeyEvent) -> bool {
    (matches!(key.code, KeyCode::Char('v' | 'V'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT))
        || (key.code == KeyCode::Insert && key.modifiers == KeyModifiers::SHIFT)
}

#[cfg(windows)]
pub fn read() -> Result<zeroize::Zeroizing<String>, &'static str> {
    use windows_sys::Win32::System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    };
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    struct Locked(windows_sys::Win32::Foundation::HGLOBAL);
    impl Drop for Locked {
        fn drop(&mut self) {
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }
    // CF_UNICODETEXT is the native NUL-terminated UTF-16 text format.
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("Clipboard is busy. Try Ctrl+V again.");
        }
        let _clipboard = Clipboard;
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            return Err("Clipboard has no text. Copy the token, then press Ctrl+V.");
        }
        let bytes = GlobalSize(handle);
        if !(2..=65536).contains(&bytes) || !bytes.is_multiple_of(2) {
            return Err("Clipboard text is too large or invalid. Copy only the value you need.");
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return Err("Cannot read clipboard text. Copy it again and retry.");
        }
        let _locked = Locked(handle);
        let units = std::slice::from_raw_parts(pointer.cast::<u16>(), bytes / 2);
        let end = units
            .iter()
            .position(|c| *c == 0)
            .ok_or("Clipboard text is invalid.")?;
        String::from_utf16(&units[..end])
            .map(zeroize::Zeroizing::new)
            .map_err(|_| "Clipboard text is invalid Unicode.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_paste_shortcuts_read_clipboard() {
        for (code, modifiers) in [
            (KeyCode::Char('v'), KeyModifiers::CONTROL),
            (
                KeyCode::Char('V'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (KeyCode::Insert, KeyModifiers::SHIFT),
        ] {
            assert!(paste_key(KeyEvent::new(code, modifiers)));
        }
        assert!(!paste_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::NONE
        )));
        assert!(!paste_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }
}
