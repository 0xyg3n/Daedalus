//! Clipboard shortcuts. Windows reads and writes the system clipboard.
//! Other platforms copy with the terminal's OSC 52 sequence.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[cfg(windows)]
pub const COPY_STATUS: &str = "Copied";
#[cfg(unix)]
pub const COPY_STATUS: &str = "Copy request sent to terminal";

#[cfg(any(windows, test))]
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

const COPY_TOO_LARGE: &str = "Selection exceeds the configured clipboard copy limit. Select less text or increase the limit in Interface settings.";
fn valid_limit(limit: usize) -> Result<(), &'static str> {
    if !(2..=daedalus_tui::app::interface_settings::MAX_CLIPBOARD_BYTES as usize).contains(&limit) {
        return Err("Invalid clipboard size limit.");
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn utf16_text(text: &str, limit: usize) -> Result<zeroize::Zeroizing<Vec<u16>>, &'static str> {
    valid_limit(limit)?;
    let mut units = zeroize::Zeroizing::new(Vec::new());
    for unit in text.encode_utf16() {
        if unit == 0 {
            return Err("Clipboard text contains a NUL character.");
        }
        if units.len() >= limit / 2 - 1 {
            return Err(COPY_TOO_LARGE);
        }
        units.push(unit);
    }
    units.push(0);
    Ok(units)
}

#[cfg(any(windows, test))]
fn read_size(bytes: usize, limit: usize) -> Result<(), &'static str> {
    valid_limit(limit)?;
    if bytes > limit {
        return Err(
            "Clipboard text exceeds the configured paste limit. Copy less text or increase the limit in Interface settings.",
        );
    }
    if bytes < 2 || !bytes.is_multiple_of(2) {
        return Err("Clipboard text buffer is invalid.");
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn decode_text(units: &[u16]) -> Result<zeroize::Zeroizing<String>, &'static str> {
    let end = units
        .iter()
        .position(|c| *c == 0)
        .ok_or("Clipboard text is invalid.")?;
    String::from_utf16(&units[..end])
        .map(zeroize::Zeroizing::new)
        .map_err(|_| "Clipboard text is invalid Unicode.")
}

#[cfg(all(windows, not(test)))]
pub fn read(limit: usize) -> Result<zeroize::Zeroizing<String>, &'static str> {
    valid_limit(limit)?;
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
        read_size(bytes, limit)?;
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return Err("Cannot read clipboard text. Copy it again and retry.");
        }
        let _locked = Locked(handle);
        let units = std::slice::from_raw_parts(pointer.cast::<u16>(), bytes / 2);
        decode_text(units)
    }
}

#[cfg(all(windows, test))]
pub fn read(_limit: usize) -> Result<zeroize::Zeroizing<String>, &'static str> {
    Err("System clipboard is not read by isolated unit tests.")
}

#[cfg(all(windows, not(test)))]
pub fn write(text: &str, limit: usize) -> Result<(), &'static str> {
    use windows_sys::Win32::{
        Foundation::{GlobalFree, HANDLE},
        System::{
            Console::GetConsoleWindow,
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
    };
    struct Allocation(windows_sys::Win32::Foundation::HGLOBAL);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    const CF_UNICODETEXT: u32 = 13;
    let units = utf16_text(text, limit)?;
    let bytes = units.len() * 2;
    unsafe {
        // Prepare memory before acquiring/emptying the clipboard. Refused text
        // and allocation failures leave its previous contents alone.
        let handle = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes);
        if handle.is_null() {
            return Err("Could not copy.");
        }
        let allocation = Allocation(handle);
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return Err("Could not copy.");
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer.cast(), units.len());
        GlobalUnlock(handle);
        // EmptyClipboard with a NULL owner makes SetClipboardData fail. A TUI
        // has a console (also a message-window handle under ConPTY).
        let owner = GetConsoleWindow();
        if owner.is_null() {
            return Err("No console clipboard owner is available.");
        }
        if OpenClipboard(owner) == 0 {
            return Err("Clipboard is busy. Try again.");
        }
        let _clipboard = Clipboard;
        if EmptyClipboard() == 0 {
            return Err("Could not copy.");
        }
        if SetClipboardData(CF_UNICODETEXT, handle as HANDLE).is_null() {
            return Err("Could not copy.");
        }
        // Ownership transfers to Windows only on successful SetClipboardData.
        std::mem::forget(allocation);
        Ok(())
    }
}

#[cfg(all(unix, not(test)))]
pub fn write(text: &str, limit: usize) -> Result<(), &'static str> {
    use std::io::Write;
    utf8_text(text, limit)?;
    let payload = crate::embed::b64_encode(text.as_bytes());
    let mut out = std::io::stdout().lock();
    write!(out, "\x1b]52;c;{payload}\x1b\\").map_err(|_| "Could not copy")?;
    out.flush().map_err(|_| "Could not copy")?;
    Ok(())
}

#[cfg(unix)]
fn utf8_text(text: &str, limit: usize) -> Result<(), &'static str> {
    valid_limit(limit)?;
    if text.len() > limit {
        return Err(COPY_TOO_LARGE);
    }
    if text.contains('\0') {
        return Err("Clipboard text contains a NUL character.");
    }
    Ok(())
}

#[cfg(test)]
pub fn write(text: &str, limit: usize) -> Result<(), &'static str> {
    // Check the production encoding policy without changing a user's clipboard.
    #[cfg(windows)]
    utf16_text(text, limit)?;
    #[cfg(unix)]
    utf8_text(text, limit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copy_keeps_complete_unicode_and_rejects_overflow_before_transfer() {
        let limit = daedalus_tui::app::interface_settings::InterfaceSettings::default()
            .clipboard_write_limit_bytes as usize;
        let text = format!("{}🚀", "a".repeat(limit / 2 - 3));
        let encoded = utf16_text(&text, limit).unwrap();
        assert_eq!(encoded.len() * 2, limit);
        assert_eq!(encoded[encoded.len() - 1], 0);
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            text
        );
        let overflow = format!("{}🚀", "a".repeat(limit / 2 - 2));
        assert_eq!(utf16_text(&overflow, limit).unwrap_err(), COPY_TOO_LARGE);
        assert!(
            utf16_text("private\0value", limit)
                .unwrap_err()
                .contains("NUL")
        );
        assert_eq!(utf16_text("🚀", 6).unwrap().as_slice(), [0xd83d, 0xde80, 0]);
        assert!(utf16_text("🚀", 5).is_err());
        assert_eq!(utf16_text("", 2).unwrap().as_slice(), [0]);
        assert!(utf16_text("x", 1).is_err());
    }
    #[test]
    fn paste_checks_buffer_budget_terminator_and_unicode() {
        assert!(read_size(6, 6).is_ok());
        assert!(read_size(8, 6).unwrap_err().contains("paste limit"));
        assert!(read_size(1, 6).is_err());
        assert!(read_size(3, 6).is_err());
        assert!(read_size(2, 1).is_err());
        assert_eq!(
            decode_text(&[0xd83d, 0xde80, 0, 0xffff]).unwrap().as_str(),
            "🚀"
        );
        assert!(decode_text(&[0xd83d, 0]).is_err());
        assert!(decode_text(&[b'x' as u16]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn osc_copy_refuses_oversized_text_instead_of_emitting_partial_utf8() {
        assert!(write("🚀", 4).is_ok());
        assert_eq!(write("🚀", 3).unwrap_err(), COPY_TOO_LARGE);
        assert!(write("x\0y", 4).is_err());
    }
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
