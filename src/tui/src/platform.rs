//! Native operating-system boundaries. Private Windows state uses explicit
//! owner-only ACLs; Unix permission bits must not be emulated as fake metadata.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
#[cfg(unix)]
mod process;
#[cfg(unix)]
pub use process::*;

/// Pipe-based helpers must not allocate a Windows console or take desktop focus.
/// Interactive terminals and the installer keep their own launch policy.
pub fn hide_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    let _ = command;
}
#[cfg(unix)]
pub fn sync_directory(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}
#[cfg(unix)]
pub fn replace_private(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}
#[cfg(unix)]
pub fn random_bytes(bytes: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(bytes)
}

/// Windows environment names are case insensitive, unlike BTreeMap keys.
/// Keep the same explicit runtime allowlists at each launch boundary.
pub fn environment() -> std::collections::BTreeMap<String, String> {
    // Unix permits non-UTF-8 environment bytes. Unrelated entries must not
    // panic discovery or the TUI; our explicit launch maps accept text only.
    let text = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)));
    #[cfg(unix)]
    let values = text.collect();
    #[cfg(windows)]
    let mut values: std::collections::BTreeMap<String, String> = text
        .map(|(key, value)| (key.to_ascii_uppercase(), value))
        .collect();
    #[cfg(windows)]
    if !values.contains_key("HOME")
        && let Some(home) = values.get("USERPROFILE").cloned()
    {
        values.insert("HOME".into(), home);
    }
    values
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, process::Command};
    #[test]
    #[ignore = "isolated child fixture, executed by the non-UTF-8 environment regression"]
    fn environment_child_fixture() {
        let values = environment();
        assert_eq!(
            values.get("DAEDALUS_TEST_ENV_VALID").map(String::as_str),
            Some("kept")
        );
        assert!(!values.contains_key("DAEDALUS_TEST_ENV_INVALID"));
    }
    #[test]
    fn non_utf8_environment_entries_cannot_crash_discovery() {
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::tests::environment_child_fixture",
                "--ignored",
            ])
            .env("DAEDALUS_TEST_ENV_VALID", "kept")
            .env("DAEDALUS_TEST_ENV_INVALID", OsString::from_vec(vec![0xff]))
            .env(OsString::from_vec(vec![b'x', 0xff]), "invalid-name")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "Isolated environment fixture failed"
        );
    }
}
