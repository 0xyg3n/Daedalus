//! Platform-native sample paths for isolated tests. This module never ships in
//! the application binary and never creates or inspects a user's directories.
pub(crate) fn path(input: &'static str) -> &'static str {
    #[cfg(unix)]
    {
        input
    }
    #[cfg(windows)]
    {
        if !input.starts_with('/') {
            return input;
        }
        let native = std::env::temp_dir()
            .join("daedalus-test-paths")
            .join(input.trim_start_matches('/'));
        Box::leak(native.to_string_lossy().into_owned().into_boxed_str())
    }
}
