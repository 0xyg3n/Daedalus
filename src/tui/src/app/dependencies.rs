//! Reviewed native dependency operations. No command strings or imported installers.
use crate::adapters::{harnesses, process};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Environment {
    pub os: String,
    pub architecture: String,
    pub home: PathBuf,
    pub search: Vec<PathBuf>,
    pub binaries: BTreeMap<String, PathBuf>,
    pub root: bool,
}
impl Environment {
    /// Explicit observation only; ignores relative/empty PATH entries.
    pub fn detect(home: &Path, search: Vec<PathBuf>) -> Result<Self> {
        valid_path(home)?;
        let mut search: Vec<_> = search.into_iter().filter(|p| p.is_absolute()).collect();
        for extra in [
            ".local/bin",
            ".grok/bin",
            ".hermes/bin",
            ".hermes/hermes-agent/venv/bin",
            ".opencode/bin",
        ] {
            let path = home.join(extra);
            if !search.contains(&path) {
                search.push(path);
            }
        }
        let mut names: BTreeSet<&str> = [
            "tmux", "herdr", "node", "npm", "apt-get", "dnf", "pacman", "sudo", "bash", "sh",
            "curl", "git", "tar", "gzip", "unzip",
        ]
        .into_iter()
        .collect();
        for tool in harnesses::tools() {
            names.extend(tool.executables);
        }
        let binaries = names
            .into_iter()
            .filter_map(|name| find(&search, name).map(|path| (name.into(), path)))
            .collect();
        Ok(Self {
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            home: home.into(),
            search,
            binaries,
            root: unsafe { libc::geteuid() } == 0,
        })
    }
    pub fn validate(&self) -> Result<()> {
        if self.os != "linux" || !["x86_64", "aarch64"].contains(&self.architecture.as_str()) {
            return Err("Native installation supports Linux x86_64 and aarch64".into());
        }
        valid_path(&self.home)?;
        for path in self.search.iter().chain(self.binaries.values()) {
            valid_path(path)?;
        }
        Ok(())
    }
}
fn valid_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("Expected an absolute normalized path".into());
    }
    Ok(())
}
fn find(paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    paths.iter().map(|p| p.join(name)).find(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Selection {
    pub tmux: bool,
    pub herdr: bool,
    pub harnesses: Vec<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Operation {
    pub installer_url: Option<String>,
    pub script_sha256: Option<String>,
    pub component: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub expected_binaries: Vec<String>,
    pub source: String,
    pub system_packages: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub environment: Environment,
    pub selection: Selection,
    pub installed: Vec<String>,
    pub operations: Vec<Operation>,
    pub blockers: Vec<String>,
    pub notices: Vec<String>,
}
pub struct Plan {
    scripts: BTreeMap<String, Vec<u8>>,
    review: Review,
    token: String,
}
impl Plan {
    pub fn prepare(environment: Environment, selection: Selection) -> Result<Self> {
        environment.validate()?;
        if !selection.tmux && !selection.herdr {
            return Err("Select tmux, Herdr, or both".into());
        }
        let mut seen = BTreeSet::new();
        for id in &selection.harnesses {
            if harnesses::tool(id).is_none() || !seen.insert(id) {
                return Err("Unknown or duplicate harness selection".into());
            }
        }
        let mut review = Review { environment, selection, installed: vec![], operations: vec![], blockers: vec![], notices: vec!["Account sign-in remains with each harness; Daedalus does not inspect credentials or launch agents. Publisher installers may access existing native profiles.".into(), "Package installation can execute publisher package scripts. Completed installs are not rolled back if a later step fails. Script digests bind the bootstrap script only; publisher downloads and package versions follow the official current distribution.".into(), "The CLI asks sudo to authenticate through its terminal only after approval; noninteractive callers must prepare sudo credentials separately.".into(), "No Python runtime is required by Daedalus; a selected third-party harness may have its own runtime requirements.".into()] };
        let mut packages = Vec::new();
        if review.selection.tmux {
            if review.environment.binaries.contains_key("tmux") {
                review.installed.push("tmux".into());
            } else {
                packages.push("tmux".into());
            }
        }
        if review.selection.herdr {
            if review.environment.binaries.contains_key("herdr") {
                review.installed.push("herdr".into());
            } else {
                add_script(
                    &mut review,
                    "herdr",
                    "herdr",
                    "https://herdr.dev/install.sh",
                    "sh",
                    vec![],
                );
            }
        }
        let prefix = review.environment.home.join(".local");
        let mut script_requests = Vec::new();
        for id in &review.selection.harnesses {
            let tool = harnesses::tool(id).expect("validated harness");
            if tool
                .executables
                .iter()
                .any(|name| review.environment.binaries.contains_key(*name))
            {
                review.installed.push(id.clone());
                continue;
            }
            let recipe = match id.as_str() {
                "claude" => Some((
                    "@anthropic-ai/claude-code",
                    "https://docs.anthropic.com/en/docs/claude-code/getting-started",
                )),
                "codex" => Some(("@openai/codex", "https://github.com/openai/codex")),
                "pi" => Some(("@earendil-works/pi-coding-agent", "https://pi.dev/")),
                "opencode" => Some(("opencode-ai", "https://opencode.ai/docs/")),
                _ => None,
            };
            let Some((package, source)) = recipe else {
                let (url, args) = match id.as_str() {
                    "cursor" => ("https://cursor.com/install", vec![]),
                    "grok" => ("https://x.ai/cli/install.sh", vec![]),
                    "hermes" => (
                        "https://hermes-agent.nousresearch.com/install.sh",
                        vec![
                            "--non-interactive".into(),
                            "--skip-browser".into(),
                            "--skip-computer-use".into(),
                        ],
                    ),
                    _ => unreachable!(),
                };
                script_requests.push((id.clone(), tool.executables[0], url, args));
                continue;
            };
            let npm = review
                .environment
                .binaries
                .get("npm")
                .cloned()
                .unwrap_or_else(|| PathBuf::from("/usr/bin/npm"));
            if !review.environment.binaries.contains_key("npm") {
                packages.push("npm".into());
            }
            if !review.environment.binaries.contains_key("node") {
                packages.push("nodejs".into());
            }
            let mut arguments = vec![
                "install".into(),
                "--global".into(),
                "--prefix".into(),
                prefix.to_string_lossy().into_owned(),
                "--registry=https://registry.npmjs.org".into(),
            ];
            if id == "pi" {
                arguments.push("--ignore-scripts".into());
            }
            arguments.push(package.into());
            review.operations.push(Operation {
                installer_url: None,
                script_sha256: None,
                component: id.clone(),
                executable: npm.clone(),
                arguments,
                expected_binaries: vec![tool.executables[0].into()],
                source: source.into(),
                system_packages: false,
            });
        }
        for (id, executable, url, args) in script_requests {
            add_script(&mut review, &id, executable, url, "bash", args);
        }
        if review
            .operations
            .iter()
            .any(|op| op.installer_url.is_some())
        {
            for dependency in ["curl", "git", "tar", "gzip", "unzip", "bash"] {
                if !review.environment.binaries.contains_key(dependency) {
                    packages.push(dependency.into());
                }
            }
        }
        packages.sort();
        packages.dedup();
        if !packages.is_empty() {
            let manager = ["apt-get", "dnf", "pacman"]
                .into_iter()
                .find(|name| review.environment.binaries.contains_key(*name));
            if let Some(manager) = manager {
                let mut arguments: Vec<String> = match manager {
                    "apt-get" => vec!["install", "-y"],
                    "dnf" => vec!["install", "-y"],
                    _ => vec!["-S", "--needed", "--noconfirm"],
                }
                .into_iter()
                .map(str::to_owned)
                .collect();
                arguments.extend(packages.clone());
                let mut executable = review.environment.binaries[manager].clone();
                if !review.environment.root {
                    if let Some(sudo) = review.environment.binaries.get("sudo") {
                        arguments.splice(
                            0..0,
                            ["-n".into(), executable.to_string_lossy().into_owned()],
                        );
                        executable = sudo.clone();
                    } else {
                        review.blockers.push("System dependencies require root or sudo; no elevation tool was found.".into());
                    }
                }
                review.operations.insert(
                    0,
                    Operation {
                        installer_url: None,
                        script_sha256: None,
                        component: "terminal dependencies".into(),
                        executable,
                        arguments,
                        expected_binaries: packages
                            .iter()
                            .map(|name| {
                                if name == "nodejs" {
                                    "node".into()
                                } else {
                                    name.clone()
                                }
                            })
                            .collect(),
                        source: format!("Configured {manager} system repositories"),
                        system_packages: true,
                    },
                );
            } else {
                review.blockers.push(
                    "Install missing system dependencies with your package manager, then refresh."
                        .into(),
                );
            }
        }
        if let Some(operation) = review.operations.first().filter(|op| {
            op.system_packages && op.source == "Configured apt-get system repositories"
        }) {
            let mut update = operation.clone();
            update.component = "package index".into();
            update.expected_binaries.clear();
            let position = update
                .arguments
                .iter()
                .position(|arg| arg == "install")
                .expect("apt install argv");
            update.arguments.truncate(position);
            update.arguments.push("update".into());
            review.operations.insert(0, update);
        }
        let token = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&review).map_err(|_| "Cannot encode install review")?
            )
        );
        Ok(Self {
            review,
            token,
            scripts: BTreeMap::new(),
        })
    }
    /// Fetch immutable installer bytes for review; never executes downloaded content.
    pub fn prepare_installers(&mut self, fetcher: &mut impl Fetcher) -> Result<()> {
        let mut staged = BTreeMap::new();
        for operation in &self.review.operations {
            if let Some(url) = &operation.installer_url {
                let bytes = fetcher.fetch(url)?;
                if bytes.is_empty()
                    || bytes.len() > 1024 * 1024
                    || !bytes.starts_with(b"#!")
                    || bytes.contains(&0)
                {
                    return Err("Invalid or oversized installer script".into());
                }
                staged.insert(operation.component.clone(), bytes);
            }
        }
        for operation in &mut self.review.operations {
            if let Some(bytes) = staged.get(&operation.component) {
                operation.script_sha256 = Some(format!("{:x}", Sha256::digest(bytes)));
            }
        }
        self.review
            .blockers
            .retain(|message| !message.starts_with("Download and review installer:"));
        self.scripts = staged;
        self.token = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&self.review).map_err(|_| "Cannot encode install review")?
            )
        );
        Ok(())
    }
    pub fn installer_script(&self, component: &str) -> Option<&[u8]> {
        self.scripts.get(component).map(Vec::as_slice)
    }
    pub fn review(&self) -> &Review {
        &self.review
    }
    /// UI must present review() before asking the user to approve this exact token.
    pub fn approval_token(&self) -> &str {
        &self.token
    }
    pub fn apply(&self, approved: &str, executor: &mut impl Executor) -> Result<InstallResult> {
        if approved != self.token {
            return Err("Installation approval does not match this plan".into());
        }
        if !self.review.blockers.is_empty() {
            return Err(
                "Resolve dependency blockers and review a new plan before installing".into(),
            );
        }
        executor.before_install(&self.review)?;
        let mut result = InstallResult {
            complete: true,
            steps: vec![],
        };
        for operation in &self.review.operations {
            let present = !operation.expected_binaries.is_empty()
                && operation
                    .expected_binaries
                    .iter()
                    .all(|name| executor.installed(name, &self.review.environment));
            let state = if present {
                "already installed"
            } else if executor
                .execute_reviewed(
                    operation,
                    &self.review.environment,
                    self.scripts.get(&operation.component).map(Vec::as_slice),
                )
                .is_err()
            {
                "installer failed"
            } else if !operation
                .expected_binaries
                .iter()
                .all(|name| executor.installed(name, &self.review.environment))
            {
                "binary missing after installation"
            } else {
                "installed"
            };
            result.steps.push(Step {
                component: operation.component.clone(),
                state: state.into(),
            });
            if !["installed", "already installed"].contains(&state) {
                result.complete = false;
                break;
            }
        }
        // Previously installed binaries may have disappeared since review.
        for id in &self.review.installed {
            let exists = if let Some(tool) = harnesses::tool(id) {
                tool.executables
                    .iter()
                    .any(|name| executor.installed(name, &self.review.environment))
            } else {
                executor.installed(id, &self.review.environment)
            };
            if !exists {
                result.complete = false;
                result.steps.push(Step {
                    component: id.clone(),
                    state: "previously observed binary is now missing".into(),
                });
            }
        }
        Ok(result)
    }
}
#[derive(Debug, Serialize)]
pub struct Step {
    pub component: String,
    pub state: String,
}
#[derive(Debug, Serialize)]
pub struct InstallResult {
    pub complete: bool,
    pub steps: Vec<Step>,
}
pub trait Executor {
    fn before_install(&mut self, _review: &Review) -> Result<()> {
        Ok(())
    }
    fn installed(&mut self, binary: &str, environment: &Environment) -> bool;
    fn execute(&mut self, operation: &Operation, environment: &Environment) -> Result<()>;
    fn execute_reviewed(
        &mut self,
        operation: &Operation,
        environment: &Environment,
        script: Option<&[u8]>,
    ) -> Result<()> {
        if script.is_some() {
            return Err("Executor does not support reviewed scripts".into());
        }
        self.execute(operation, environment)
    }
}
pub struct NativeExecutor;
impl Executor for NativeExecutor {
    fn installed(&mut self, binary: &str, environment: &Environment) -> bool {
        let mut paths = environment.search.clone();
        paths.insert(0, environment.home.join(".local/bin"));
        for path in [
            ".grok/bin",
            ".hermes/bin",
            ".hermes/hermes-agent/venv/bin",
            ".opencode/bin",
        ] {
            paths.push(environment.home.join(path));
        }
        find(&paths, binary).is_some()
    }
    fn execute_reviewed(
        &mut self,
        operation: &Operation,
        environment: &Environment,
        script: Option<&[u8]>,
    ) -> Result<()> {
        let Some(bytes) = script else {
            return self.execute(operation, environment);
        };
        if operation.script_sha256.as_deref()
            != Some(format!("{:x}", Sha256::digest(bytes)).as_str())
        {
            return Err("Installer does not match reviewed digest".into());
        }
        let stage = ScriptStage::create(bytes)?;
        let mut command = operation.clone();
        command
            .arguments
            .insert(0, stage.0.join("installer").to_string_lossy().into_owned());
        self.execute(&command, environment)
    }
    fn execute(&mut self, operation: &Operation, environment: &Environment) -> Result<()> {
        environment.validate()?;
        let empty_npm_config = ScriptStage::create(b"")?;
        let mut command = Command::new(&operation.executable);
        let mut search = environment.search.clone();
        search.insert(0, environment.home.join(".local/bin"));
        command
            .args(&operation.arguments)
            .env_clear()
            .env("HOME", &environment.home)
            .env(
                "PATH",
                std::env::join_paths(search).map_err(|_| "Invalid executable search path")?,
            )
            .env("LANG", "C.UTF-8")
            .env("NPM_CONFIG_USERCONFIG", "/dev/null")
            .env(
                "NPM_CONFIG_GLOBALCONFIG",
                empty_npm_config.0.join("installer"),
            )
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("CI", "1")
            .env("GROK_BIN_DIR", environment.home.join(".local/bin"))
            .current_dir(&environment.home);
        let output = process::run(&mut command, Duration::from_secs(900), 4 * 1024 * 1024)
            .map_err(|_| "Installer could not complete; review dependencies and retry")?;
        if output.status.success() {
            Ok(())
        } else {
            Err("Installer reported failure; configuration was not activated".into())
        }
    }
}

/// CLI-only elevation prompt after the exact plan has been approved. Passwords
/// are handled by sudo through its inherited terminal, never a Rust buffer.
pub struct CliExecutor;
fn sudo_authentication(review: &Review) -> Option<Command> {
    let operation = review.operations.iter().find(|op| {
        op.system_packages
            && op.executable.file_name().is_some_and(|name| name == "sudo")
            && op.arguments.first().is_some_and(|arg| arg == "-n")
    })?;
    let mut command = Command::new(&operation.executable);
    command
        .arg("-v")
        .env_clear()
        .env("HOME", &review.environment.home)
        .env(
            "PATH",
            std::env::join_paths(&review.environment.search).ok()?,
        )
        .env("TERM", "xterm")
        .current_dir(&review.environment.home);
    Some(command)
}
impl Executor for CliExecutor {
    fn before_install(&mut self, review: &Review) -> Result<()> {
        let Some(mut command) = sudo_authentication(review) else {
            return Ok(());
        };
        // Preserve the foreground terminal. The ordinary process runner supplies
        // null stdin and therefore cannot perform this interactive credential step.
        let status = command
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|_| "Cannot authenticate sudo; no packages were installed")?;
        if status.success() {
            Ok(())
        } else {
            Err("Sudo authentication failed or was cancelled; no packages were installed".into())
        }
    }
    fn installed(&mut self, binary: &str, environment: &Environment) -> bool {
        NativeExecutor.installed(binary, environment)
    }
    fn execute(&mut self, operation: &Operation, environment: &Environment) -> Result<()> {
        NativeExecutor.execute(operation, environment)
    }
    fn execute_reviewed(
        &mut self,
        operation: &Operation,
        environment: &Environment,
        script: Option<&[u8]>,
    ) -> Result<()> {
        NativeExecutor.execute_reviewed(operation, environment, script)
    }
}

fn add_script(
    review: &mut Review,
    component: &str,
    binary: &str,
    url: &str,
    shell: &str,
    arguments: Vec<String>,
) {
    review
        .blockers
        .push(format!("Download and review installer: {component}"));
    let executable = review
        .environment
        .binaries
        .get(shell)
        .cloned()
        .unwrap_or_else(|| PathBuf::from(format!("/bin/{shell}")));
    review.operations.push(Operation {
        component: component.into(),
        executable,
        arguments,
        expected_binaries: vec![binary.into()],
        source: url.into(),
        system_packages: false,
        installer_url: Some(url.into()),
        script_sha256: None,
    });
}
pub trait Fetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>>;
}
pub struct HttpsFetcher;
impl Fetcher for HttpsFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>> {
        use std::io::Read;
        let allowed = [
            "https://herdr.dev/install.sh",
            "https://cursor.com/install",
            "https://x.ai/cli/install.sh",
            "https://hermes-agent.nousresearch.com/install.sh",
        ];
        if !allowed.contains(&url) {
            return Err("Unknown installer origin".into());
        }
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| "Cannot initialize installer download")?;
        let response = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| "Installer download failed")?;
        let mut bytes = Vec::new();
        response
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read installer download")?;
        if bytes.len() > 1024 * 1024 {
            return Err("Installer download exceeds limit".into());
        }
        Ok(bytes)
    }
}
struct ScriptStage(PathBuf);
impl ScriptStage {
    fn create(bytes: &[u8]) -> Result<Self> {
        use std::{
            io::Write,
            os::unix::fs::{DirBuilderExt, OpenOptionsExt},
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "daedalus-install-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "Cannot create private installer stage")?;
        let stage = Self(path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(stage.0.join("installer"))
            .map_err(|_| "Cannot stage reviewed installer")?;
        file.write_all(bytes)
            .map_err(|_| "Cannot save reviewed installer")?;
        Ok(stage)
    }
}
impl Drop for ScriptStage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env() -> Environment {
        Environment {
            os: "linux".into(),
            architecture: "x86_64".into(),
            home: "/private/user".into(),
            search: vec!["/usr/bin".into()],
            binaries: BTreeMap::from([
                ("apt-get".into(), "/usr/bin/apt-get".into()),
                ("sudo".into(), "/usr/bin/sudo".into()),
            ]),
            root: false,
        }
    }
    #[test]
    fn architecture_and_selection_fail_closed() {
        let selection = Selection {
            tmux: true,
            herdr: false,
            harnesses: vec![],
        };
        for (os, arch) in [("linux", "riscv64"), ("macos", "aarch64")] {
            let mut e = env();
            e.os = os.into();
            e.architecture = arch.into();
            assert!(Plan::prepare(e, selection.clone()).is_err());
        }
        for harnesses in [vec!["voice".into()], vec!["codex".into(), "codex".into()]] {
            assert!(
                Plan::prepare(
                    env(),
                    Selection {
                        harnesses,
                        ..selection.clone()
                    }
                )
                .is_err()
            );
        }
        assert!(
            Plan::prepare(
                env(),
                Selection {
                    tmux: false,
                    herdr: false,
                    harnesses: vec![]
                }
            )
            .is_err()
        );
    }
    #[test]
    fn system_install_uses_exact_argv_and_explicit_elevation() {
        let plan = Plan::prepare(
            env(),
            Selection {
                tmux: true,
                herdr: false,
                harnesses: vec![],
            },
        )
        .unwrap();
        let operation = &plan.review().operations[1];
        assert_eq!(operation.executable, PathBuf::from("/usr/bin/sudo"));
        assert_eq!(
            operation.arguments,
            vec!["-n", "/usr/bin/apt-get", "install", "-y", "tmux"]
        );
        assert!(plan.review().blockers.is_empty());
        let mut e = env();
        e.binaries.remove("sudo");
        assert!(
            !Plan::prepare(
                e,
                Selection {
                    tmux: true,
                    herdr: false,
                    harnesses: vec![]
                }
            )
            .unwrap()
            .review()
            .blockers
            .is_empty()
        );
    }
    #[test]
    fn npm_prerequisites_and_manual_installers_are_not_fabricated() {
        let plan = Plan::prepare(
            env(),
            Selection {
                tmux: true,
                herdr: true,
                harnesses: vec![
                    "codex".into(),
                    "cursor".into(),
                    "grok".into(),
                    "hermes".into(),
                ],
            },
        )
        .unwrap();
        assert_eq!(plan.review().blockers.len(), 4);
        assert_eq!(plan.review().operations.len(), 7);
        assert!(
            plan.review().operations[1]
                .arguments
                .contains(&"nodejs".into())
        );
    }
    #[test]
    fn native_executor_uses_literal_args_scrubbed_environment_and_checks_exit() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "daedalus-dependencies-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let script = directory.join("fixture");
        std::fs::write(&script,"#!/bin/sh\n[ \"$1\" = '$(do-not-execute)' ] || exit 1\n[ \"${OPENAI_API_KEY+x}\" != x ] || exit 2\n[ \"$NPM_CONFIG_USERCONFIG\" = /dev/null ] || exit 3\n[ \"$2\" != fail ]\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut e = env();
        e.home = directory.clone();
        let mut op = Operation {
            installer_url: None,
            script_sha256: None,
            component: "fixture".into(),
            executable: script,
            arguments: vec!["$(do-not-execute)".into()],
            expected_binaries: vec![],
            source: "isolated test fixture".into(),
            system_packages: false,
        };
        let mut executor = NativeExecutor;
        assert!(executor.execute(&op, &e).is_ok());
        op.arguments.push("fail".into());
        assert!(executor.execute(&op, &e).is_err());
        let bytes = b"#!/bin/sh\n[ \"$1\" = '$(do-not-execute)' ]\n";
        op.arguments = vec!["$(do-not-execute)".into()];
        op.executable = "/bin/sh".into();
        op.script_sha256 = Some(format!("{:x}", Sha256::digest(bytes)));
        assert!(executor.execute_reviewed(&op, &e, Some(bytes)).is_ok());
        assert!(!executor.installed("nonexistent-daedalus-fixture", &e));
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn installer_download_requires_new_digest_approval() {
        struct Fixture;
        impl Fetcher for Fixture {
            fn fetch(&mut self, _: &str) -> Result<Vec<u8>> {
                Ok(b"#!/bin/sh\nexit 0\n".to_vec())
            }
        }
        let mut plan = Plan::prepare(
            env(),
            Selection {
                tmux: true,
                herdr: true,
                harnesses: vec![],
            },
        )
        .unwrap();
        let old = plan.approval_token().to_owned();
        assert!(
            plan.review()
                .blockers
                .iter()
                .any(|s| s.starts_with("Download"))
        );
        plan.prepare_installers(&mut Fixture).unwrap();
        assert_ne!(old, plan.approval_token());
        assert!(plan.review().blockers.is_empty());
        assert_eq!(
            plan.review().operations[2]
                .script_sha256
                .as_ref()
                .unwrap()
                .len(),
            64
        );
        assert!(plan.apply(&old, &mut NativeExecutor).is_err());
        let mut operation = plan.review().operations[2].clone();
        operation.script_sha256 = Some("tampered".into());
        assert!(
            NativeExecutor
                .execute_reviewed(&operation, &env(), Some(b"#!/bin/sh\nexit 0\n"))
                .is_err()
        );
    }
    #[test]
    fn elevation_is_exact_native_command_and_only_after_matching_approval() {
        let plan = Plan::prepare(
            env(),
            Selection {
                tmux: true,
                herdr: false,
                harnesses: vec![],
            },
        )
        .unwrap();
        let command = sudo_authentication(plan.review()).unwrap();
        assert_eq!(command.get_program(), "/usr/bin/sudo");
        assert_eq!(command.get_args().collect::<Vec<_>>(), vec!["-v"]);
        struct Denied {
            auth: usize,
            runs: usize,
        }
        impl Executor for Denied {
            fn before_install(&mut self, _: &Review) -> Result<()> {
                self.auth += 1;
                Err("fixture authentication cancelled".into())
            }
            fn installed(&mut self, _: &str, _: &Environment) -> bool {
                false
            }
            fn execute(&mut self, _: &Operation, _: &Environment) -> Result<()> {
                self.runs += 1;
                Ok(())
            }
        }
        let mut executor = Denied { auth: 0, runs: 0 };
        assert!(plan.apply("not approved", &mut executor).is_err());
        assert_eq!(executor.auth, 0);
        assert!(plan.apply(plan.approval_token(), &mut executor).is_err());
        assert_eq!(executor.auth, 1);
        assert_eq!(executor.runs, 0);
    }
}
