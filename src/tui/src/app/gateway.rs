//! Optional documented provider overrides. The launcher reads a private credential
//! only immediately before exec; no secret is placed in a tmux command or plan.
//! Provider contracts: https://code.claude.com/docs/en/llm-gateway-connect and
//! https://developers.openai.com/codex/config-advanced/ (custom model providers).
use super::config::ControlConfig;
use crate::{
    adapters::harnesses,
    core::{checkpoints::LaunchPlan, layouts},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};
#[cfg(unix)]
use std::{fs::OpenOptions, io::Read};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    AnthropicMessages,
    OpenaiResponses,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub enabled: bool,
    pub format: Format,
    pub endpoint: String,
    pub model: String,
    pub credential_file: PathBuf,
    pub harness: String,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !matches!(
                (&self.format, self.harness.as_str()),
                (Format::AnthropicMessages, "claude") | (Format::OpenaiResponses, "codex")
            )
        {
            return Err("Unsupported gateway format and harness combination".into());
        }
        let url = reqwest::Url::parse(&self.endpoint).map_err(|_| "Invalid gateway endpoint")?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || self.endpoint.len() > 2048
            || !(url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(
                        url.host_str(),
                        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                    )))
        {
            return Err("Gateway requires HTTPS (HTTP only on loopback), no credentials or query in the URL".into());
        }
        if self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err("Invalid gateway model".into());
        }
        layouts::check_path(&self.credential_file)
            .map_err(|_| "Gateway credential path must be absolute and symlink-free")?;
        Ok(())
    }
}
#[cfg(unix)]
fn private_bytes(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    layouts::check_path(path).map_err(|_| "Unsafe gateway path")?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read private gateway state".into()),
    };
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect gateway state")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || meta.len() > limit
    {
        return Err("Gateway state must be a bounded private owned regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read gateway state")?;
    if bytes.len() as u64 > limit {
        return Err("Gateway state exceeds limit".into());
    }
    Ok(Some(bytes))
}
pub fn load(directory: &Path) -> Result<Option<Config>> {
    let Some(bytes) = private_bytes(&directory.join("gateway.json"), 16384)? else {
        return Ok(None);
    };
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid gateway configuration")?;
    config.validate()?;
    Ok(Some(config))
}
fn fingerprint(config: &Config) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(config).map_err(|_| "Cannot encode gateway configuration")?
        )
    ))
}
fn credential(config: &Config) -> Result<String> {
    let bytes = private_bytes(&config.credential_file, 8192)?
        .ok_or("Gateway credential file is missing")?;
    let value = String::from_utf8(bytes).map_err(|_| "Invalid gateway credential encoding")?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err("Gateway credential must contain one nonempty token".into());
    }
    Ok(value.into())
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self.0);
    }
}
pub fn save(directory: &Path, expected: &Option<Config>, updated: &Config) -> Result<()> {
    updated.validate()?;
    #[cfg(unix)]
    let file = {
        let meta = std::fs::symlink_metadata(directory)
            .map_err(|_| "Gateway instance directory is missing")?;
        layouts::check_path(directory)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("Gateway requires a private owned instance directory".into());
        }
        let path = directory.join("gateway.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "Cannot lock gateway configuration")?;
        let meta = file.metadata().map_err(|_| "Cannot inspect gateway lock")?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err("Unsafe gateway lock".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another gateway update is active".into());
        }
        file
    };
    #[cfg(windows)]
    let file = {
        crate::platform::validate_private_dir(directory).map_err(|e| e.to_string())?;
        let file = crate::platform::open_private(&directory.join("gateway.lock"), true, true)
            .map_err(|e| e.to_string())?;
        crate::platform::lock_exclusive(&file, true)
            .map_err(|_| "Another gateway update is active")?;
        file
    };
    let _lock = Lock(file);
    if &load(directory)? != expected {
        return Err("Gateway configuration changed; review it again".into());
    }
    if updated.enabled {
        let mut token = credential(updated)?.into_bytes();
        token.fill(0);
    }
    layouts::write_private(
        &directory.join("gateway.json"),
        &serde_json::to_vec_pretty(updated).map_err(|_| "Cannot encode gateway configuration")?,
    )
}
/// Wrap a terminal launch with a credential-free argv. The hash rejects edits
/// between launch preparation and exec; native account environment is retained.
pub fn wrap(
    directory: &Path,
    harness: &str,
    launch: &mut LaunchPlan,
    executable: &Path,
) -> Result<()> {
    let Some(config) = load(directory)? else {
        return Ok(());
    };
    if !config.enabled || config.harness != harness {
        return Ok(());
    }
    let args = vec![
        "gateway-exec".into(),
        "--config-dir".into(),
        directory.to_string_lossy().into_owned(),
        "--harness".into(),
        harness.into(),
        "--expected".into(),
        fingerprint(&config)?,
        "--binary".into(),
        launch.binary.to_string_lossy().into_owned(),
        "--".into(),
    ];
    launch.args = args.into_iter().chain(launch.args.clone()).collect();
    launch.binary = executable.into();
    Ok(())
}
/// Apply only documented API-format overrides; existing sign-in is never used
/// as a gateway credential. Secret values live only in the child environment.
pub fn apply(plan: &mut harnesses::ProcessPlan, config: &Config) -> Result<()> {
    config.validate()?;
    let secret = credential(config)?;
    match config.format {
        Format::AnthropicMessages => {
            plan.environment
                .insert("ANTHROPIC_BASE_URL".into(), config.endpoint.clone());
            plan.environment
                .insert("ANTHROPIC_AUTH_TOKEN".into(), secret);
            plan.environment
                .insert("ANTHROPIC_MODEL".into(), config.model.clone());
        }
        Format::OpenaiResponses => {
            plan.environment
                .insert("DAEDALUS_GATEWAY_TOKEN".into(), secret);
            let quote = |value: &str| serde_json::to_string(value).unwrap();
            let options = [
                format!("model={}", quote(&config.model)),
                "model_provider=\"daedalus_gateway\"".into(),
                "model_providers.daedalus_gateway.name=\"Daedalus gateway\"".into(),
                format!(
                    "model_providers.daedalus_gateway.base_url={}",
                    quote(&config.endpoint)
                ),
                "model_providers.daedalus_gateway.env_key=\"DAEDALUS_GATEWAY_TOKEN\"".into(),
                "model_providers.daedalus_gateway.wire_api=\"responses\"".into(),
                "model_providers.daedalus_gateway.requires_openai_auth=false".into(),
            ];
            let mut args = Vec::new();
            for option in options {
                args.extend(["-c".into(), option]);
            }
            args.append(&mut plan.args);
            plan.args = args;
        }
    }
    Ok(())
}
pub fn run(args: &[String]) -> Result<()> {
    let mut directory = None;
    let mut harness = None;
    let mut expected = None;
    let mut binary = None;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--" {
            rest = args[index + 1..].to_vec();
            break;
        }
        let value = args
            .get(index + 1)
            .ok_or("Missing gateway launcher argument")?
            .clone();
        match args[index].as_str() {
            "--config-dir" if directory.is_none() => directory = Some(PathBuf::from(value)),
            "--harness" if harness.is_none() => harness = Some(value),
            "--expected" if expected.is_none() => expected = Some(value),
            "--binary" if binary.is_none() => binary = Some(PathBuf::from(value)),
            _ => return Err("Unknown gateway launcher argument".into()),
        };
        index += 2;
    }
    let directory = directory.ok_or("Missing gateway configuration directory")?;
    let gateway = load(&directory)?.ok_or("Gateway configuration is missing")?;
    if !gateway.enabled
        || Some(&gateway.harness) != harness.as_ref()
        || Some(fingerprint(&gateway)?) != expected
    {
        return Err("Gateway launch configuration changed".into());
    }
    let _config = ControlConfig::parse(
        &layouts::read_private(&directory.join("control.json"))?
            .ok_or("Control configuration is missing")?,
    )?;
    let environment: BTreeMap<String, String> = crate::platform::environment();
    let tool = harnesses::tool(&gateway.harness).ok_or("Unknown gateway harness")?;
    let found =
        harnesses::executable(&tool, &environment).ok_or("Gateway harness is not installed")?;
    if Some(&found) != binary.as_ref() {
        return Err("Gateway harness executable changed".into());
    }
    let cwd = std::env::current_dir().map_err(|_| "Cannot inspect gateway launch directory")?;
    let mut plan = harnesses::launch_plan(&tool, &found, &cwd, &environment, &BTreeMap::new())?;
    plan.args = rest;
    apply(&mut plan, &gateway)?;
    #[cfg(unix)]
    let _error = plan.command().exec();
    #[cfg(windows)]
    {
        let status = plan
            .command()
            .status()
            .map_err(|_| "Cannot launch native provider")?;
        if status.success() {
            return Ok(());
        }
        Err(format!("Native provider exited with {status}"))
    }
    #[cfg(unix)]
    Err("Gateway harness did not start".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-gateway-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            layouts::write_private(&path.join("token"), b"PRIVATE-KEY\n").unwrap();
            Self(path)
        }
        fn config(&self) -> Config {
            Config {
                version: 1,
                enabled: true,
                format: Format::AnthropicMessages,
                endpoint: "https://gateway.example.invalid".into(),
                model: "configured-model".into(),
                credential_file: self.0.join("token"),
                harness: "claude".into(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn configuration_is_explicit_private_compare_and_swap_and_rejects_unsafe_urls() {
        let f = Fixture::new();
        let original = f.config();
        save(&f.0, &None, &original).unwrap();
        assert_eq!(load(&f.0).unwrap(), Some(original.clone()));
        let mut disabled = original.clone();
        disabled.enabled = false;
        assert!(save(&f.0, &None, &disabled).is_err());
        save(&f.0, &Some(original.clone()), &disabled).unwrap();
        assert!(
            !fs::read_to_string(f.0.join("gateway.json"))
                .unwrap()
                .contains("PRIVATE-KEY")
        );
        for endpoint in [
            "http://remote.example.invalid",
            "https://user:password@example.invalid",
            "https://example.invalid?key=secret",
            "file:///tmp/token",
        ] {
            let mut bad = original.clone();
            bad.endpoint = endpoint.into();
            assert!(bad.validate().is_err());
        }
        let mut local = original;
        local.endpoint = "http://127.0.0.1:8080/v1".into();
        assert!(local.validate().is_ok());
    }
    #[test]
    fn gateway_wrap_is_credential_free_and_documented_overrides_are_child_only() {
        let f = Fixture::new();
        let config = f.config();
        save(&f.0, &None, &config).unwrap();
        let environment = [
            ("HOME".into(), f.0.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                crate::test_support::path("/usr/bin:/bin").into(),
            ),
        ]
        .into_iter()
        .collect();
        let mut plan = harnesses::launch_plan(
            &harnesses::tool("claude").unwrap(),
            Path::new("/mock/claude"),
            &f.0,
            &environment,
            &BTreeMap::new(),
        )
        .unwrap();
        let mut launch = LaunchPlan {
            operation: crate::core::checkpoints::Operation::Handoff,
            binary: plan.binary.clone(),
            args: vec!["literal $(touch nope)".into()],
            environment: plan.environment.clone(),
            cwd: f.0.clone(),
        };
        wrap(&f.0, "claude", &mut launch, Path::new("/mock/daedalus")).unwrap();
        assert_eq!(launch.args[0], "gateway-exec");
        assert!(!format!("{:?}{:?}", launch.args, launch.environment).contains("PRIVATE-KEY"));
        apply(&mut plan, &config).unwrap();
        assert_eq!(plan.environment["ANTHROPIC_AUTH_TOKEN"], "PRIVATE-KEY");
        assert!(!format!("{plan:?}").contains("PRIVATE-KEY"));
        let mut codex = config;
        codex.harness = "codex".into();
        codex.format = Format::OpenaiResponses;
        let mut plan = harnesses::launch_plan(
            &harnesses::tool("codex").unwrap(),
            Path::new("/mock/codex"),
            &f.0,
            &environment,
            &BTreeMap::new(),
        )
        .unwrap();
        apply(&mut plan, &codex).unwrap();
        assert_eq!(plan.environment["DAEDALUS_GATEWAY_TOKEN"], "PRIVATE-KEY");
        assert!(
            plan.args
                .iter()
                .any(|a| a == "model_providers.daedalus_gateway.wire_api=\"responses\"")
        );
        assert!(!plan.args.join(" ").contains("PRIVATE-KEY"));
    }
    #[test]
    fn credentials_reject_symlinks_public_permissions_and_nonfiles() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let mut config = f.config();
        let link = f.0.join("link");
        symlink(&config.credential_file, &link).unwrap();
        config.credential_file = link;
        assert!(credential(&config).is_err());
        config.credential_file = f.0.join("token");
        fs::set_permissions(&config.credential_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(credential(&config).is_err());
        config.credential_file = f.0.clone();
        assert!(credential(&config).is_err());
    }
}

#[cfg(windows)]
fn private_bytes(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    layouts::read_private_limit(path, limit)
}
