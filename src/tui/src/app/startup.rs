//! Native dashboard startup selection. Resolving a launch never writes state.
//! Building the backend command on Windows rewrites managed hook profiles that
//! still name a temporary setup executable. It does not start or stop Telegram.
use crate::core::layouts;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;
/// Stable preference root, independent of the currently selected instance.
pub(crate) fn preference_directory(
    cwd: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    let home = if cfg!(windows) {
        env("USERPROFILE").or_else(|| env("HOME"))
    } else {
        env("HOME").or_else(|| env("USERPROFILE"))
    }
    .map(PathBuf::from);
    let mut directory = if let Some(path) = env("DAEDALUS_CONFIG_DIR") {
        PathBuf::from(path)
    } else if let Some(root) = env("XDG_CONFIG_HOME") {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err("XDG_CONFIG_HOME must be absolute".into());
        }
        root.join("daedalus")
    } else if cfg!(windows) && env("APPDATA").is_some() {
        let root = PathBuf::from(env("APPDATA").unwrap());
        if !root.is_absolute() {
            return Err("APPDATA must be absolute".into());
        }
        root.join("Daedalus")
    } else {
        home.as_ref()
            .ok_or("Set HOME or use --config-dir ABS")?
            .join(".config/daedalus")
    };
    if directory.as_os_str().is_empty() {
        return Err("Configuration path must not be empty".into());
    }
    if directory.starts_with("~") {
        directory = home
            .clone()
            .ok_or("HOME required to expand configuration path")?
            .join(
                directory
                    .strip_prefix("~")
                    .map_err(|_| "Invalid configuration path")?,
            );
    }
    if !directory.is_absolute() {
        directory = if env("DAEDALUS_CONFIG_DIR").is_some() {
            home.as_ref()
                .ok_or("HOME required for a relative DAEDALUS_CONFIG_DIR")?
                .join(directory)
        } else {
            cwd.join(directory)
        };
    }
    validate(&directory)?;
    Ok(directory)
}
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub directory: Option<PathBuf>,
    pub first_run: bool,
    pub workspace: PathBuf,
}
impl Launch {
    pub fn resolve(
        args: &[String],
        cwd: &Path,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        let mut demo = false;
        let mut explicit = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--demo" if !demo => demo = true,
                "--config-dir" if explicit.is_none() => {
                    explicit = Some(PathBuf::from(
                        args.next().ok_or("--config-dir requires a path")?,
                    ));
                }
                _ => return Err("Unknown or repeated backend startup option".into()),
            }
        }
        if demo {
            return Ok(Self {
                directory: None,
                first_run: false,
                workspace: cwd.into(),
            });
        }
        let home = if cfg!(windows) {
            env("USERPROFILE").or_else(|| env("HOME"))
        } else {
            env("HOME").or_else(|| env("USERPROFILE"))
        }
        .map(PathBuf::from);
        let mut directory = match explicit.as_ref() {
            Some(path) => path.clone(),
            None => preference_directory(cwd, &env)?,
        };
        if directory.as_os_str().is_empty() {
            return Err("Configuration path must not be empty".into());
        }
        if directory.starts_with("~") {
            directory = home
                .clone()
                .ok_or("HOME required to expand configuration path")?
                .join(
                    directory
                        .strip_prefix("~")
                        .map_err(|_| "Invalid configuration path")?,
                );
        }
        if !directory.is_absolute() {
            directory = cwd.join(directory);
        }
        validate(&directory)?;
        if explicit.is_none()
            && let Some(remembered) = super::instances::remembered(&directory)?
        {
            directory = remembered;
        }
        let config = layouts::read_private(&directory.join("control.json"))?;
        if let Some(bytes) = &config {
            super::config::ControlConfig::parse(bytes)?;
        }
        let first_run = config.is_none();
        // Start Menu and installer launches inherit an arbitrary working
        // directory. Use the user's profile as the initial Windows workspace.
        let workspace = if cfg!(windows) {
            home.as_deref().filter(|p| p.is_absolute()).unwrap_or(cwd)
        } else {
            cwd
        };
        if first_run {
            validate(workspace)?;
        }
        Ok(Self {
            directory: Some(directory),
            first_run,
            workspace: workspace.into(),
        })
    }
    /// The frontend and backend are the same installed executable, never PATH resolution.
    /// A Windows setup package runs from a temporary directory. When the installed
    /// file exists, the backend is that file and managed hooks are pointed at it.
    /// Rebind failures are not reported: this function cannot fail without changing
    /// the TUI launcher, and it never starts or stops Telegram.
    pub fn command(&self, executable: &OsStr) -> Command {
        let program = self.runtime_program(executable);
        let mut command = Command::new(&program);
        command.args(["backend", "--stdio"]);
        if let Some(directory) = &self.directory {
            command.arg("--config-dir").arg(directory);
            if self.first_run {
                command
                    .arg("--first-run")
                    .arg("--workspace")
                    .arg(&self.workspace);
            }
        } else {
            command.arg("--demo");
        }
        command
    }

    fn runtime_program(&self, executable: &OsStr) -> OsString {
        #[cfg(windows)]
        if let Some(directory) = &self.directory
            && let Some(installed) = installed_executable()
        {
            // The launcher ignores the error. `rebind_temporary_hook_commands` is the
            // checked API; a failed rewrite leaves the previous hook command in place.
            let _ = rebind_temporary_hook_commands(directory, &installed);
            return windows_runtime_executable(Path::new(executable), Some(&installed))
                .into_os_string();
        }
        executable.to_os_string()
    }
}

/// Installed copy under the user local data directory, when that file exists.
#[cfg(windows)]
fn installed_executable() -> Option<PathBuf> {
    let path = super::windows_install::destination().ok()?;
    if path.is_file() && !temporary_setup_executable(&path) {
        Some(path)
    } else {
        None
    }
}

/// True for an NSIS plugin directory, update cache, or install temp file.
/// A normal `%LOCALAPPDATA%\Daedalus\bin\daedalus.exe` path is false.
pub fn temporary_setup_executable(path: &Path) -> bool {
    path.to_str().is_some_and(temporary_setup_executable_str)
}

fn temporary_setup_executable_str(text: &str) -> bool {
    let stripped = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix("//?/"))
        .unwrap_or(text);
    let normalized = stripped.replace('/', "\\");
    let parts: Vec<&str> = normalized
        .split('\\')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.iter().any(|part| *part == "." || *part == "..") {
        return false;
    }
    let Some(name) = parts.last() else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    if (name.starts_with(".daedalus-install-") || name.starts_with("daedalus-"))
        && name.ends_with(".tmp")
    {
        return true;
    }
    // The NSIS package and a replaced image are not hook runtimes.
    if name == "daedalus-setup.exe"
        || (name.ends_with(".exe.old") && name.contains("daedalus"))
    {
        return true;
    }
    if !matches!(
        name.as_str(),
        "daedalus.exe" | "daedalus" | "daedalus-tui.exe"
    ) {
        return false;
    }
    let parent = parts
        .get(parts.len().wrapping_sub(2))
        .copied()
        .unwrap_or("")
        .to_ascii_lowercase();
    parent == "temp"
        || parent == "tmp"
        || nsis_plugin_dir(&parent)
        || parts
            .iter()
            .any(|part| part.eq_ignore_ascii_case(".daedalus-updates"))
}

fn nsis_plugin_dir(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("ns") && name.ends_with(".tmp") && name.len() > 6
}

fn absolute_executable(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return path.is_absolute();
    };
    let bytes = text.as_bytes();
    path.is_absolute()
        || text.starts_with(r"\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

/// Use the installed executable only when the running path is a temporary setup copy.
pub fn windows_runtime_executable(running: &Path, installed: Option<&Path>) -> PathBuf {
    if let Some(installed) = installed
        && temporary_setup_executable(running)
        && !temporary_setup_executable(installed)
        && absolute_executable(installed)
    {
        return installed.to_path_buf();
    }
    running.to_path_buf()
}

/// Replace temporary setup executables inside one hook profile.
/// Other commands, including registration paths that merely live under Temp, stay.
/// `Ok(None)` means the profile already names no temporary setup executable.
pub fn rewrite_temporary_hook_paths(
    profile: &str,
    installed: &Path,
) -> Result<Option<String>> {
    let installed_text = installed
        .to_str()
        .filter(|text| !text.is_empty())
        .ok_or("Installed hook executable path is not UTF-8")?;
    if !absolute_executable(installed) || temporary_setup_executable(installed) {
        return Err("Refusing to bind hooks to a temporary setup executable".into());
    }
    if installed_text
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\''))
    {
        return Err("Installed hook executable path cannot be embedded safely".into());
    }
    let mut replacements = Vec::new();
    for (start, end) in executable_spans(profile) {
        let raw = &profile[start..end];
        let body = raw.strip_prefix(r"\\?\").unwrap_or(raw);
        let decoded = decode_hook_path(body);
        let temporary = temporary_setup_executable_str(body)
            || decoded
                .as_deref()
                .is_some_and(temporary_setup_executable_str);
        if !temporary {
            continue;
        }
        let Some(_) = decoded else {
            return Err("Temporary hook path uses mixed separators".into());
        };
        let Some(replacement) = encode_like(body, installed_text) else {
            return Err("Temporary hook path uses mixed separators".into());
        };
        if replacement != raw {
            replacements.push((start, end, replacement));
        }
    }
    if replacements.is_empty() {
        return Ok(None);
    }
    let mut text = profile.to_string();
    for (start, end, replacement) in replacements.into_iter().rev() {
        text.replace_range(start..end, &replacement);
    }
    Ok(Some(text))
}

fn decode_hook_path(raw: &str) -> Option<String> {
    let slash = raw.matches('\\').count();
    let doubled = raw.matches("\\\\").count();
    if doubled > 0 {
        if slash != doubled * 2 {
            return None;
        }
        return Some(raw.replace("\\\\", "\\"));
    }
    Some(raw.to_string())
}

fn encode_like(raw: &str, installed: &str) -> Option<String> {
    let slash = raw.matches('\\').count();
    let doubled = raw.matches("\\\\").count();
    if doubled > 0 {
        if slash != doubled * 2 {
            return None;
        }
        return Some(installed.replace('\\', "\\\\").replace('/', "\\\\"));
    }
    if raw.contains('/') && !raw.contains('\\') {
        return Some(installed.replace('\\', "/"));
    }
    Some(installed.replace('/', "\\"))
}

fn executable_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        let drive = bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && (bytes[index + 2] == b'\\' || bytes[index + 2] == b'/')
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric());
        if !drive {
            index += 1;
            continue;
        }
        let mut start = index;
        if start >= 4 && &bytes[start - 4..start] == br"\\?\" {
            start -= 4;
        }
        let mut end = index + 3;
        while end < bytes.len() {
            let byte = bytes[end];
            if byte < 0x20 || matches!(byte, b'"' | b'\'' | b'<' | b'>' | b'|' | b'*' | b'?') {
                break;
            }
            end += 1;
        }
        if let Some(relative) = executable_end(&text[start..end]) {
            spans.push((start, start + relative));
            index = start + relative;
        } else {
            index = end.max(index + 1);
        }
    }
    spans
}

fn executable_end(span: &str) -> Option<usize> {
    let lower = span.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    for marker in [
        "daedalus-setup.exe",
        "daedalus-tui.exe",
        "daedalus.exe",
        "daedalus",
    ] {
        let mut search = 0;
        while let Some(relative) = lower[search..].find(marker) {
            let at = search + relative;
            let end = at + marker.len();
            let before_ok = at == 0 || matches!(bytes[at - 1], b'\\' | b'/');
            let next = bytes.get(end).copied();
            let after_ok = next.is_none_or(|byte| !byte.is_ascii_alphanumeric());
            let short_prefix = marker == "daedalus" && next.is_some_and(|byte| byte == b'.' || byte == b'-');
            if before_ok && after_ok && !short_prefix {
                best = Some(match best {
                    Some((old_at, old_end)) if old_at < at || (old_at == at && old_end >= end) => {
                        (old_at, old_end)
                    }
                    _ => (at, end),
                });
                break;
            }
            search = at + 1;
        }
    }
    let mut search = 0;
    while let Some(relative) = lower[search..].find(".daedalus-install-") {
        let at = search + relative;
        let before_ok = at == 0 || matches!(bytes[at - 1], b'\\' | b'/');
        if before_ok
            && let Some(tmp) = lower[at..].find(".tmp")
        {
            let end = at + tmp + 4;
            let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if after_ok {
                best = Some(match best {
                    Some((old_at, old_end)) if old_at < at || (old_at == at && old_end >= end) => {
                        (old_at, old_end)
                    }
                    _ => (at, end),
                });
            }
        }
        search = at + 1;
    }
    best.map(|(_, end)| end)
}

/// Point managed hook profiles at `installed` when they still name a temporary
/// setup executable. Receipts outside this instance are rejected. Telegram is
/// not started or stopped. The original profile bytes are kept under `hook-reloads`
/// before a write; an original install receipt then no longer matches `after_sha`,
/// so hook rollback refuses until the profile is reviewed again.
#[cfg(windows)]
pub fn rebind_temporary_hook_commands(directory: &Path, installed: &Path) -> Result<usize> {
    use sha2::Digest;
    if temporary_setup_executable(installed) || !absolute_executable(installed) {
        return Err("Refusing to bind hooks to a temporary setup executable".into());
    }
    let root = directory.join("hook-backups");
    if !root.exists() {
        return Ok(0);
    }
    let _lock = super::hook_install::lock(directory)?;
    let profiles = receipt_profiles(directory, &root)?;
    let mut pending = Vec::new();
    for profile in profiles {
        let Some(bytes) = super::hook_install::read_profile(&profile)? else {
            continue;
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| "Hook profile is not UTF-8")?;
        if let Some(rewritten) = rewrite_temporary_hook_paths(text, installed)? {
            pending.push((profile, bytes, rewritten));
        }
    }
    for (profile, before, rewritten) in &pending {
        let parent = profile.parent().ok_or("Hook profile has no parent")?;
        let _profile_lock = if parent != directory {
            Some(super::hook_install::lock_profile(parent)?)
        } else {
            None
        };
        let record = serde_json::json!({
            "profile": profile,
            "before": before,
            "after_sha": format!("{:x}", sha2::Sha256::digest(rewritten.as_bytes())),
        });
        let encoded =
            serde_json::to_vec(&record).map_err(|_| "Cannot encode hook rebind backup")?;
        let key = format!("{:x}", sha2::Sha256::digest(&encoded));
        layouts::write_private(
            &directory.join("hook-reloads").join(format!("{key}.json")),
            &encoded,
        )?;
        super::hook_install::write_profile(profile, rewritten.as_bytes())?;
    }
    Ok(pending.len())
}

#[cfg(windows)]
fn receipt_profiles(directory: &Path, root: &Path) -> Result<Vec<PathBuf>> {
    let hooks = directory.join("hooks");
    let mut profiles = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|_| "Cannot read managed hook receipts")?;
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err("Managed hook receipt limit exceeded".into());
        }
        let path = entry.map_err(|_| "Cannot read hook receipt")?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".receipt.json") {
            continue;
        }
        layouts::check_path(&path)?;
        let Some(bytes) = layouts::read_private(&path)? else {
            continue;
        };
        let receipt: super::hook_install::Receipt =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid managed hook receipt")?;
        if receipt.registration.parent() != Some(hooks.as_path()) {
            return Err("Hook receipt points outside this instance".into());
        }
        layouts::check_path(&receipt.profile)?;
        if !profiles.contains(&receipt.profile) {
            profiles.push(receipt.profile);
        }
    }
    Ok(profiles)
}

fn validate(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err("Invalid configuration path".into());
    }
    layouts::check_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_config_override_is_stable_across_launch_directories() {
        let home = PathBuf::from(crate::test_support::path("/tmp/daedalus-profile"));
        let env = |key: &str| match key {
            "HOME" | "USERPROFILE" => Some(home.as_os_str().to_owned()),
            "DAEDALUS_CONFIG_DIR" => Some(OsString::from("my-daedalus")),
            _ => None,
        };
        let first = preference_directory(
            Path::new(crate::test_support::path("/tmp/project-one")),
            env,
        )
        .unwrap();
        let second = preference_directory(
            Path::new(crate::test_support::path("/tmp/project-two")),
            env,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first, home.join("my-daedalus"));
    }
    #[test]
    fn remembered_instance_is_used_only_without_an_explicit_override() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "daedalus-startup-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let base = root.join(".config/daedalus");
        let saved = root.join("saved-instance");
        let config = super::super::config::ControlConfig::fresh(&root).unwrap();
        for dir in [&base, &saved] {
            layouts::write_private(
                &dir.join("control.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
        }
        let preference = serde_json::to_vec(&serde_json::json!({"last_instance":saved})).unwrap();
        layouts::write_private(&base.join("dashboard.json"), &preference).unwrap();
        let env = |name: &str| (name == "HOME").then(|| root.as_os_str().to_owned());
        let launch = Launch::resolve(&[], &root, env).unwrap();
        assert_eq!(launch.directory, Some(saved.clone()));
        assert!(!launch.first_run);
        let explicit = Launch::resolve(
            &["--config-dir".into(), base.to_str().unwrap().into()],
            &root,
            env,
        )
        .unwrap();
        assert_eq!(explicit.directory, Some(base.clone()));
        std::fs::remove_dir_all(&saved).unwrap();
        assert_eq!(
            Launch::resolve(&[], &root, env).unwrap().directory,
            Some(base.clone())
        );
        assert_eq!(
            std::fs::read(base.join("dashboard.json")).unwrap(),
            preference
        );
        assert!(Launch::resolve(&["--config-dir".into(), "".into()], &root, env).is_err());
    }
    #[test]
    fn demo_ignores_private_paths_and_needs_no_environment() {
        let launch = Launch::resolve(
            &[
                "--demo".into(),
                "--config-dir".into(),
                "/dev/null/private".into(),
            ],
            Path::new(crate::test_support::path("/")),
            |_| None,
        )
        .unwrap();
        assert_eq!(launch.directory, None);
        let command = launch.command(OsStr::new("/installed/daedalus"));
        assert_eq!(command.get_program(), "/installed/daedalus");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["backend", "--stdio", "--demo"]
        );
    }
    #[test]
    fn explicit_configuration_requires_no_home_and_rejects_traversal() {
        let launch = Launch::resolve(
            &[
                "--config-dir".into(),
                crate::test_support::path("/daedalus-test-missing/instance").into(),
            ],
            Path::new(crate::test_support::path("/")),
            |_| None,
        )
        .unwrap();
        assert!(launch.first_run);
        assert!(
            Launch::resolve(
                &[
                    "--config-dir".into(),
                    crate::test_support::path("/tmp/../private").into()
                ],
                Path::new(crate::test_support::path("/")),
                |_| None
            )
            .is_err()
        );
        assert!(Launch::resolve(&[], Path::new(crate::test_support::path("/")), |_| None).is_err());
        let configured_env =
            Launch::resolve(&[], Path::new(crate::test_support::path("/")), |name| {
                (name == "DAEDALUS_CONFIG_DIR").then(|| {
                    OsString::from(crate::test_support::path("/daedalus-test-missing/instance"))
                })
            })
            .unwrap();
        assert_eq!(configured_env.directory, launch.directory);
    }
    #[test]
    fn xdg_and_explicit_overrides_keep_existing_instances_distinct() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-xdg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let legacy = root.join(".config/daedalus");
        let xdg = root.join("xdg/daedalus");
        let config = super::super::config::ControlConfig::fresh(&root).unwrap();
        for path in [&legacy, &xdg] {
            layouts::write_private(
                &path.join("control.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
        }
        let env = |name: &str| match name {
            "HOME" => Some(root.clone().into_os_string()),
            "XDG_CONFIG_HOME" => Some(root.join("xdg").into_os_string()),
            _ => None,
        };
        assert_eq!(
            Launch::resolve(&[], &root, env).unwrap().directory,
            Some(xdg)
        );
        assert_eq!(
            Launch::resolve(
                &["--config-dir".into(), legacy.to_str().unwrap().into()],
                &root,
                env
            )
            .unwrap()
            .directory,
            Some(legacy.clone())
        );
        assert_eq!(
            Launch::resolve(&[], &root, |name| (name == "HOME")
                .then(|| root.clone().into_os_string()))
            .unwrap()
            .directory,
            Some(legacy)
        );
        assert!(
            Launch::resolve(&[], &root, |name| (name == "XDG_CONFIG_HOME")
                .then(|| "relative".into()))
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn temporary_setup_hook_paths_rebind_to_the_installed_executable() {
        let installed = PathBuf::from(r"C:\Users\Public\AppData\Local\Daedalus\bin\daedalus.exe");
        let temp = PathBuf::from(r"C:\Users\Public\AppData\Local\Temp\nsA1B2.tmp\daedalus.exe");
        let updates = PathBuf::from(
            r"C:\Users\Public\AppData\Local\Daedalus\.daedalus-updates\abc\daedalus.exe",
        );
        let copy = PathBuf::from(r"C:\Users\Temp\AppData\Local\Daedalus\bin\daedalus.exe");
        assert!(temporary_setup_executable(&temp));
        assert!(temporary_setup_executable(&updates));
        assert!(temporary_setup_executable(Path::new(
            r"C:\Users\Public\AppData\Local\Daedalus\bin\.daedalus-install-aa.tmp"
        )));
        assert!(temporary_setup_executable(Path::new(
            r"C:\Users\Public\AppData\Local\Daedalus\bin\daedalus.exe.old"
        )));
        assert!(!temporary_setup_executable(&installed));
        assert!(!temporary_setup_executable(&copy));
        assert_eq!(
            windows_runtime_executable(&temp, Some(&installed)),
            installed
        );
        assert_eq!(
            windows_runtime_executable(&installed, Some(&installed)),
            installed
        );
        assert_eq!(
            windows_runtime_executable(&temp, Some(&updates)),
            temp,
            "a temporary path is never the hook target"
        );
        let profile = r#"{"command":"C:\\Users\\Public\\AppData\\Local\\Temp\\nsA1B2.tmp\\daedalus.exe","args":["hook","run","--registration","C:\\Users\\Public\\AppData\\Local\\Temp\\hooks.json","--event-kind","Stop"],"user":"C:\\Users\\Temp\\AppData\\Local\\Daedalus\\bin\\daedalus.exe"}"#;
        let rewritten = rewrite_temporary_hook_paths(profile, &installed)
            .unwrap()
            .unwrap();
        assert!(rewritten.contains(r"C:\\Users\\Public\\AppData\\Local\\Daedalus\\bin\\daedalus.exe"));
        assert!(!rewritten.contains("nsA1B2.tmp"));
        assert!(rewritten.contains(r"C:\\Users\\Public\\AppData\\Local\\Temp\\hooks.json"));
        assert!(rewritten.contains(r"C:\\Users\\Temp\\AppData\\Local\\Daedalus\\bin\\daedalus.exe"));
        assert!(!rewritten.contains("serve"));
        assert!(rewrite_temporary_hook_paths(&rewritten, &installed)
            .unwrap()
            .is_none());
        let forward = r#"{"binary":"C:/Users/Public/AppData/Local/Temp/daedalus.exe"}"#;
        let forward = rewrite_temporary_hook_paths(forward, &installed)
            .unwrap()
            .unwrap();
        assert_eq!(
            forward,
            r#"{"binary":"C:/Users/Public/AppData/Local/Daedalus/bin/daedalus.exe"}"#
        );
    }
}
