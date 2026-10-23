//! Clipboard shortcuts. Windows reads and writes the system clipboard.
//! Other platforms copy with the terminal's OSC 52 sequence.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn paste_key(key: KeyEvent) -> bool {
    (matches!(key.code, KeyCode::Char('v' | 'V'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT))
        || (key.code == KeyCode::Insert && key.modifiers == KeyModifiers::SHIFT)
}

/// Ctrl+C and Ctrl+Shift+C. A bare Ctrl+C still interrupts when nothing is selected.
pub fn copy_key(key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    match key.code {
        KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('\u{3}') => true,
        _ => false,
    }
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

#[cfg(windows)]
pub fn write(text: &str) -> Result<(), &'static str> {
    use windows_sys::Win32::{
        Foundation::{GlobalFree, HANDLE},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
    };
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    const CF_UNICODETEXT: u32 = 13;
    let mut units: Vec<u16> = text.encode_utf16().take(256 * 1024).collect();
    units.push(0);
    let bytes = units.len() * 2;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("Clipboard is busy. Try again.");
        }
        let _clipboard = Clipboard;
        if EmptyClipboard() == 0 {
            return Err("Could not copy.");
        }
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if handle.is_null() {
            return Err("Could not copy.");
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            GlobalFree(handle);
            return Err("Could not copy.");
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer.cast(), units.len());
        GlobalUnlock(handle);
        if SetClipboardData(CF_UNICODETEXT, handle as HANDLE).is_null() {
            GlobalFree(handle);
            return Err("Could not copy.");
        }
        Ok(())
    }
}

#[cfg(all(unix, not(test)))]
pub fn write(text: &str) -> Result<(), &'static str> {
    use std::io::Write;
    let payload = crate::embed::b64_encode(text.as_bytes());
    let mut out = std::io::stdout().lock();
    write!(out, "\x1b]52;c;{payload}\x1b\\").map_err(|_| "Could not copy")?;
    out.flush().map_err(|_| "Could not copy")?;
    Ok(())
}

#[cfg(all(unix, test))]
pub fn write(text: &str) -> Result<(), &'static str> {
    let _ = text;
    Ok(())
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

    #[test]
    fn copy_key_is_ctrl_c_and_not_alt() {
        assert!(copy_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(copy_key(KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(copy_key(KeyEvent::new(
            KeyCode::Char('\u{3}'),
            KeyModifiers::NONE
        )));
        assert!(!copy_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }
}
