//! Platform-native sample paths for isolated tests. This module never ships in
//! the application binary and never creates or inspects a user's directories.
pub(crate) fn path(input: &'static str) -> &'static str {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        input
    }
    #[cfg(target_os = "macos")]
    {
        // macOS /tmp and /var are symlinks. The application rejects symlinked
        // state paths, so fixtures under /tmp resolve the temporary root first.
        // System paths such as /bin/sh stay unchanged.
        if input == "/tmp" || input.starts_with("/tmp/") {
            let base = std::env::temp_dir();
            let base = std::fs::canonicalize(&base).unwrap_or(base);
            let suffix = input.strip_prefix("/tmp").unwrap_or(input);
            let base = base.join("daedalus-test-paths");
            let native = if suffix.is_empty() {
                base
            } else {
                base.join(suffix.trim_start_matches('/'))
            };
            Box::leak(native.to_string_lossy().into_owned().into_boxed_str())
        } else {
            input
        }
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
