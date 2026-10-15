//! Native operating-system boundaries. Private Windows state uses explicit
//! owner-only ACLs; Unix permission bits must not be emulated as fake metadata.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
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
    #[cfg(unix)]
    let values = std::env::vars().collect();
    #[cfg(windows)]
    let mut values: std::collections::BTreeMap<String, String> = std::env::vars()
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
