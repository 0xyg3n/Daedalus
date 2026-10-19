//! Native dashboard startup selection. Resolving a launch never writes state.
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
    pub fn command(&self, executable: &OsStr) -> Command {
        let mut command = Command::new(executable);
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
}
