//! UI-independent setup wizard transaction: choose, review, explicitly apply, verify.
//! The existing TUI owns dialogs; this module does not draw or prompt.
use super::{
    config::ControlConfig,
    dependencies::{self, Environment, Executor, Selection},
    setup::{SetupPlan, SetupPreview, SetupResult},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub instance_import: Option<super::migration::Preview>,
    pub configuration: SetupPreview,
    pub dependencies: dependencies::Review,
    pub sign_in: Vec<String>,
}
pub struct Wizard {
    instance_import: Option<super::migration::ImportPlan>,
    configuration: SetupPlan,
    dependencies: dependencies::Plan,
    review: Review,
    token: String,
}
#[derive(Serialize)]
pub struct Applied {
    pub instance_import: Option<super::migration::Applied>,
    pub dependencies: dependencies::InstallResult,
    pub configuration: Option<SetupResult>,
    pub configuration_error: Option<String>,
    pub ready: bool,
}
impl Wizard {
    pub fn configure(
        directory: &Path,
        mut config: ControlConfig,
        environment: Environment,
        selection: Selection,
    ) -> Result<Self> {
        config.subscriptions = selection.harnesses.clone();
        Self::prepare(SetupPlan::edit(directory, config)?, environment, selection)
    }

    pub fn fresh(
        directory: &Path,
        workspace: &Path,
        environment: Environment,
        selection: Selection,
    ) -> Result<Self> {
        let mut config = ControlConfig::fresh(workspace)?;
        config.subscriptions = selection.harnesses.clone();
        Self::prepare(SetupPlan::fresh(directory, config)?, environment, selection)
    }
    /// Portable settings import. Full instance state is handled by migration::ImportPlan.
    pub fn import_settings(
        directory: &Path,
        source: &Path,
        environment: Environment,
        tmux: bool,
        herdr: bool,
    ) -> Result<Self> {
        let configuration = SetupPlan::import(directory, source)?;
        let selection = Selection {
            tmux,
            herdr,
            harnesses: configuration
                .config()
                .subscriptions
                .iter()
                .filter(|id| id.as_str() != "gateway")
                .cloned()
                .collect(),
        };
        Self::prepare(configuration, environment, selection)
    }
    fn prepare(
        configuration: SetupPlan,
        environment: Environment,
        selection: Selection,
    ) -> Result<Self> {
        let dependencies = dependencies::Plan::prepare(environment, selection)?;
        let sign_in = dependencies
            .review()
            .selection
            .harnesses
            .iter()
            .filter(|id| !dependencies.review().installed.contains(id))
            .map(|id| {
                format!(
                    "{id}: open the harness after installation; it handles any required sign-in"
                )
            })
            .collect();
        let review = Review {
            instance_import: None,
            configuration: configuration.preview.clone(),
            dependencies: dependencies.review().clone(),
            sign_in,
        };
        // Bind approval to actual settings too, without publishing their contents.
        let mut digest = Sha256::new();
        digest.update(serde_json::to_vec(&review).map_err(|_| "Cannot encode wizard review")?);
        digest.update(configuration.config().encode()?);
        let token = format!("{:x}", digest.finalize());
        Ok(Self {
            instance_import: None,
            configuration,
            dependencies,
            review,
            token,
        })
    }
    pub fn import_instance(
        directory: &Path,
        source: &Path,
        environment: Environment,
        tmux: bool,
        herdr: bool,
        credentials: bool,
    ) -> Result<Self> {
        let mut wizard = Self::import_settings(
            directory,
            &source.join("control.json"),
            environment,
            tmux,
            herdr,
        )?;
        let import = super::migration::ImportPlan::prepare(source, directory, credentials)?;
        wizard.review.instance_import = Some(import.preview.clone());
        wizard.instance_import = Some(import);
        wizard.rehash()?;
        Ok(wizard)
    }
    fn rehash(&mut self) -> Result<()> {
        let mut digest = Sha256::new();
        digest.update(serde_json::to_vec(&self.review).map_err(|_| "Cannot encode wizard review")?);
        digest.update(self.configuration.config().encode()?);
        self.token = format!("{:x}", digest.finalize());
        Ok(())
    }
    pub fn prepare_installers(&mut self, fetcher: &mut impl dependencies::Fetcher) -> Result<()> {
        self.dependencies.prepare_installers(fetcher)?;
        self.review.dependencies = self.dependencies.review().clone();
        self.rehash()
    }
    pub fn installer_script(&self, component: &str) -> Option<&[u8]> {
        self.dependencies.installer_script(component)
    }
    pub fn review(&self) -> &Review {
        &self.review
    }
    pub fn approval_token(&self) -> &str {
        &self.token
    }
    pub fn apply(self, approved: &str, executor: &mut impl Executor) -> Result<Applied> {
        if approved != self.token {
            return Err("Setup approval does not match this review".into());
        }
        let dependencies = self
            .dependencies
            .apply(self.dependencies.approval_token(), executor)?;
        if !dependencies.complete {
            return Ok(Applied {
                instance_import: None,
                dependencies,
                configuration: None,
                configuration_error: None,
                ready: false,
            });
        }
        if let Some(import) = self.instance_import {
            return match import.apply() {
                Ok(imported) => Ok(Applied { dependencies, configuration: None, instance_import: Some(imported), configuration_error: None, ready: true }),
                Err(_) => Ok(Applied { dependencies, configuration: None, instance_import: None, configuration_error: Some("Instance import could not complete; review a new plan. Source files remain unchanged.".into()), ready: false }),
            };
        }
        match self.configuration.apply() {
            Ok(configuration)=>Ok(Applied {instance_import:None,dependencies,configuration:Some(configuration),configuration_error:None,ready:true}),
            Err(_)=>Ok(Applied {instance_import:None,dependencies,configuration:None,configuration_error:Some("Configuration changed or could not be saved; review a new setup plan. Completed dependency installs remain installed.".into()),ready:false}),
        }
    }
}

/// Interactive CLI adapter; account sign-in is deliberately not executed.
pub fn run(
    args: &[String],
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        return writeln!(writer,"Usage: daedalus setup --cli --config-dir ABS [--workspace ABS | --from ABS [--import-instance] [--credentials]]\nSelect backends and harnesses, review installation, then explicitly approve. No account sign-in is performed.").map_err(|_|"Cannot write setup help".into());
    }
    use std::{collections::BTreeMap, path::PathBuf};
    let mut options = BTreeMap::new();
    let mut flags = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if ["--import-instance", "--credentials"].contains(&key) {
            if !flags.insert(key) {
                return Err("Repeated setup option".into());
            }
            index += 1;
            continue;
        }
        if !["--config-dir", "--workspace", "--from"].contains(&key) || index + 1 >= args.len() {
            return Err("Usage: setup --cli --config-dir ABS [--workspace ABS | --from ABS [--import-instance] [--credentials]]".into());
        }
        if options.insert(key, args[index + 1].as_str()).is_some() {
            return Err("Repeated setup option".into());
        }
        index += 2;
    }
    let directory = Path::new(
        *options
            .get("--config-dir")
            .ok_or("Setup requires --config-dir ABS")?,
    );
    let source = options.get("--from");
    let workspace = options.get("--workspace");
    if source.is_some() == workspace.is_some()
        || (source.is_none() && !flags.is_empty())
        || (flags.contains("--credentials") && !flags.contains("--import-instance"))
    {
        return Err("Choose a workspace for fresh setup or an import source; credentials require full instance import".into());
    }
    let runtime = crate::platform::environment();
    let home = runtime
        .get("HOME")
        .map(PathBuf::from)
        .ok_or("Home directory is not configured")?;
    let search = runtime
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let environment = Environment::detect(&home, search)?;
    prompt(
        writer,
        &format!(
            "Detected: {} / {}. Native terminals are built in; existing skips extra backend installation.\nTerminal backends: existing, tmux, herdr, or both [existing]: ",
            environment.os, environment.architecture
        ),
    )?;
    let backend = read_answer(reader)?;
    let (tmux, herdr) = match backend.as_str() {
        "" | "existing" => (false, false),
        "tmux" => (true, false),
        "herdr" => (false, true),
        "both" => (true, true),
        _ => return Err("Choose existing, tmux, herdr, or both".into()),
    };
    let wizard = if let Some(source) = source {
        if flags.contains("--import-instance") {
            Wizard::import_instance(
                directory,
                Path::new(source),
                environment,
                tmux,
                herdr,
                flags.contains("--credentials"),
            )?
        } else {
            Wizard::import_settings(directory, Path::new(source), environment, tmux, herdr)?
        }
    } else {
        prompt(
            writer,
            "Select harnesses (comma-separated: claude,codex,grok,hermes,cursor,pi,opencode; empty skips): ",
        )?;
        let harnesses = read_answer(reader)?
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Wizard::fresh(
            directory,
            Path::new(workspace.expect("validated workspace")),
            environment,
            Selection {
                tmux,
                herdr,
                harnesses,
            },
        )?
    };
    run_with(
        wizard,
        reader,
        writer,
        &mut dependencies::HttpsFetcher,
        &mut dependencies::CliExecutor,
    )
}
fn prompt(writer: &mut impl std::io::Write, text: &str) -> Result<()> {
    writer
        .write_all(text.as_bytes())
        .and_then(|_| writer.flush())
        .map_err(|_| "Cannot write setup output".into())
}
fn read_answer(reader: &mut impl std::io::BufRead) -> Result<String> {
    let mut answer = Vec::new();
    loop {
        let chunk = reader.fill_buf().map_err(|_| "Cannot read setup answer")?;
        if chunk.is_empty() {
            return Err("Setup input ended; nothing further was approved".into());
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.map_or(chunk.len(), |p| p + 1);
        if answer.len() + count > 4096 {
            return Err("Setup answer exceeds limit".into());
        }
        answer.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    let answer = String::from_utf8(answer).map_err(|_| "Invalid setup answer")?;
    Ok(answer.trim().to_owned())
}
/// Deterministic dialog/installer boundary. Fixtures inject every external effect.
pub fn run_with(
    mut wizard: Wizard,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
    fetcher: &mut impl dependencies::Fetcher,
    executor: &mut impl Executor,
) -> Result<()> {
    let scripts: Vec<_> = wizard
        .review()
        .dependencies
        .operations
        .iter()
        .filter_map(|operation| operation.installer_url.clone())
        .collect();
    if !scripts.is_empty() {
        prompt(
            writer,
            &format!(
                "Installer sources to download for review (no execution):\n{}\nDownload these installers? Type yes: ",
                scripts.join("\n")
            ),
        )?;
        if read_answer(reader)? != "yes" {
            return prompt(writer, "Setup cancelled; no installers executed.\n");
        }
        wizard.prepare_installers(fetcher)?;
    }
    let review =
        super::dialog_text::format(wizard.review()).map_err(|_| "Cannot encode setup review")?;
    prompt(writer, &format!("{review}\n"))?;
    if !wizard.review().dependencies.blockers.is_empty() {
        return Err("Setup has unresolved prerequisites; no installers executed".into());
    }
    prompt(
        writer,
        "Apply this configuration and install the listed dependencies? Type yes: ",
    )?;
    if read_answer(reader)? != "yes" {
        return prompt(writer, "Setup cancelled; no changes applied.\n");
    }
    let token = wizard.approval_token().to_owned();
    let sign_in = wizard.review().sign_in.clone();
    prompt(
        writer,
        if wizard.review().dependencies.operations.is_empty() {
            "Saving reviewed configuration; no dependency installations are needed.\n"
        } else {
            "Applying reviewed plan; dependency installation may take several minutes.\n"
        },
    )?;
    let result = wizard.apply(&token, executor)?;
    prompt(
        writer,
        &format!(
            "{}\n",
            super::dialog_text::format(&result).map_err(|_| "Cannot encode setup result")?
        ),
    )?;
    if !result.ready {
        return Err("Setup did not complete; inspect the step states above before retrying".into());
    }
    for instruction in sign_in {
        prompt(writer, &format!("{instruction}\n"))?;
    }
    prompt(
        writer,
        "Configuration prepared. No feeder, agent, or account sign-in was started.\n",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "daedalus-wizard-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            }
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Fake {
        binaries: BTreeSet<String>,
        calls: usize,
        install: bool,
    }
    impl Executor for Fake {
        fn installed(&mut self, name: &str, _: &Environment) -> bool {
            self.binaries.contains(name)
        }
        fn execute(&mut self, op: &dependencies::Operation, _: &Environment) -> Result<()> {
            self.calls += 1;
            if self.install {
                self.binaries.extend(op.expected_binaries.iter().cloned());
            }
            Ok(())
        }
    }
    fn environment(home: &Path) -> Environment {
        Environment {
            os: "linux".into(),
            architecture: "aarch64".into(),
            home: home.into(),
            search: vec![crate::test_support::path("/fake/bin").into()],
            binaries: BTreeMap::from([
                (
                    "tmux".into(),
                    crate::test_support::path("/fake/bin/tmux").into(),
                ),
                (
                    "npm".into(),
                    crate::test_support::path("/fake/bin/npm").into(),
                ),
                (
                    "node".into(),
                    crate::test_support::path("/fake/bin/node").into(),
                ),
            ]),
            root: false,
        }
    }
    fn selected() -> Selection {
        Selection {
            tmux: true,
            herdr: false,
            harnesses: vec!["codex".into()],
        }
    }
    fn fake() -> Fake {
        Fake {
            binaries: BTreeSet::from(["tmux".into()]),
            calls: 0,
            install: true,
        }
    }
    #[test]
    fn fresh_review_does_nothing_then_installs_and_saves() {
        let temp = Temp::new();
        let dest = temp.0.join("instance");
        let wizard = Wizard::fresh(&dest, &temp.0, environment(&temp.0), selected()).unwrap();
        assert!(!dest.exists());
        assert_eq!(wizard.review().dependencies.operations.len(), 1);
        let token = wizard.approval_token().to_owned();
        let mut executor = fake();
        let applied = wizard.apply(&token, &mut executor).unwrap();
        assert!(applied.ready);
        assert_eq!(executor.calls, 1);
        let config =
            ControlConfig::parse(&std::fs::read(dest.join("control.json")).unwrap()).unwrap();
        assert_eq!(config.subscriptions, vec!["codex"]);
    }
    #[test]
    fn missing_postinstall_binary_prevents_config_write() {
        let temp = Temp::new();
        let dest = temp.0.join("instance");
        let wizard = Wizard::fresh(&dest, &temp.0, environment(&temp.0), selected()).unwrap();
        let token = wizard.approval_token().to_owned();
        let mut executor = fake();
        executor.install = false;
        let applied = wizard.apply(&token, &mut executor).unwrap();
        assert!(!applied.ready);
        assert!(!dest.exists());
        assert_eq!(
            applied.dependencies.steps[0].state,
            "binary missing after installation"
        );
    }
    #[test]
    fn imported_settings_keep_backend_endpoints_and_source_untouched() {
        let temp = Temp::new();
        let source = temp.0.join("old.json");
        let dest = temp.0.join("instance");
        let mut config = ControlConfig::fresh(&temp.0).unwrap();
        config.subscriptions = vec!["codex".into()];
        config.tmux_sockets = vec![crate::test_support::path("/private/custom.sock").into()];
        config.discovery.herdr_sessions = vec!["project".into()];
        let original = config.encode().unwrap();
        std::fs::write(&source, &original).unwrap();
        let wizard =
            Wizard::import_settings(&dest, &source, environment(&temp.0), true, false).unwrap();
        let token = wizard.approval_token().to_owned();
        let mut executor = fake();
        assert!(wizard.apply(&token, &mut executor).unwrap().ready);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(
            ControlConfig::parse(&std::fs::read(dest.join("control.json")).unwrap()).unwrap(),
            config
        );
    }
    #[test]
    fn wrong_approval_and_blockers_never_execute() {
        let temp = Temp::new();
        let mut executor = fake();
        let wizard = Wizard::fresh(
            &temp.0.join("instance"),
            &temp.0,
            environment(&temp.0),
            selected(),
        )
        .unwrap();
        assert!(wizard.apply("not reviewed", &mut executor).is_err());
        assert_eq!(executor.calls, 0);
        let mut selection = selected();
        selection.herdr = true;
        let wizard = Wizard::fresh(
            &temp.0.join("instance"),
            &temp.0,
            environment(&temp.0),
            selection,
        )
        .unwrap();
        let token = wizard.approval_token().to_owned();
        assert!(wizard.apply(&token, &mut executor).is_err());
        assert_eq!(executor.calls, 0);
    }
    #[test]
    fn stale_config_preserves_dependency_receipt() {
        let temp = Temp::new();
        let dest = temp.0.join("instance");
        let wizard = Wizard::fresh(&dest, &temp.0, environment(&temp.0), selected()).unwrap();
        let token = wizard.approval_token().to_owned();
        SetupPlan::fresh(&dest, ControlConfig::fresh(&temp.0).unwrap())
            .unwrap()
            .apply()
            .unwrap();
        let result = wizard.apply(&token, &mut fake()).unwrap();
        assert!(!result.ready);
        assert!(result.configuration_error.is_some());
        assert_eq!(result.dependencies.steps.len(), 1);
    }
    #[test]
    fn full_instance_import_copies_state_without_activation() {
        let temp = Temp::new();
        let source = temp.0.join("source");
        let dest = temp.0.join("destination");
        SetupPlan::fresh(&source, ControlConfig::fresh(&temp.0).unwrap())
            .unwrap()
            .apply()
            .unwrap();
        let wizard =
            Wizard::import_instance(&dest, &source, environment(&temp.0), true, false, false)
                .unwrap();
        assert!(wizard.review().instance_import.is_some());
        assert!(!dest.exists());
        let token = wizard.approval_token().to_owned();
        let result = wizard.apply(&token, &mut fake()).unwrap();
        assert!(result.ready);
        assert!(result.instance_import.unwrap().requires_cutover);
        assert!(source.join("control.json").exists());
        assert!(dest.join("control.json").exists());
    }
    #[test]
    fn injected_cli_requires_explicit_yes_and_leaves_login_native() {
        struct NoNetwork;
        impl dependencies::Fetcher for NoNetwork {
            fn fetch(&mut self, _: &str) -> Result<Vec<u8>> {
                panic!("unexpected download")
            }
        }
        let temp = Temp::new();
        let dest = temp.0.join("instance");
        let mut executor = fake();
        let mut output = Vec::new();
        let wizard = Wizard::fresh(&dest, &temp.0, environment(&temp.0), selected()).unwrap();
        run_with(
            wizard,
            &mut std::io::Cursor::new(b"no\n"),
            &mut output,
            &mut NoNetwork,
            &mut executor,
        )
        .unwrap();
        assert!(!dest.exists());
        assert_eq!(executor.calls, 0);
        let wizard = Wizard::fresh(&dest, &temp.0, environment(&temp.0), selected()).unwrap();
        run_with(
            wizard,
            &mut std::io::Cursor::new(b"yes\n"),
            &mut output,
            &mut NoNetwork,
            &mut executor,
        )
        .unwrap();
        assert!(dest.join("control.json").exists());
        assert_eq!(executor.calls, 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("handles any required sign-in"));
        assert!(output.contains("@openai/codex"));
    }
    #[test]
    fn bounded_answers_reject_eof_and_overlong_input() {
        assert!(read_answer(&mut std::io::Cursor::new(b"")).is_err());
        assert!(read_answer(&mut std::io::Cursor::new(vec![b'x'; 4097])).is_err());
    }
    #[test]
    fn injected_cli_downloads_before_review_but_runs_only_after_second_yes() {
        struct Download {
            calls: usize,
        }
        impl dependencies::Fetcher for Download {
            fn fetch(&mut self, _: &str) -> Result<Vec<u8>> {
                self.calls += 1;
                Ok(b"#!/bin/sh\nexit 0\n".to_vec())
            }
        }
        let temp = Temp::new();
        let dest = temp.0.join("instance");
        let mut environment = environment(&temp.0);
        for name in ["bash", "sh", "curl", "git", "tar", "gzip", "unzip"] {
            environment
                .binaries
                .insert(name.into(), format!("/fake/bin/{name}").into());
        }
        let wizard = Wizard::fresh(
            &dest,
            &temp.0,
            environment,
            Selection {
                tmux: true,
                herdr: true,
                harnesses: vec![],
            },
        )
        .unwrap();
        let mut fetcher = Download { calls: 0 };
        let mut executor = fake();
        let mut output = Vec::new();
        run_with(
            wizard,
            &mut std::io::Cursor::new(b"yes\nno\n"),
            &mut output,
            &mut fetcher,
            &mut executor,
        )
        .unwrap();
        assert_eq!(fetcher.calls, 1);
        assert_eq!(executor.calls, 0);
        assert!(!dest.exists());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Script SHA256:"));
        use sha2::Digest;
        assert!(output.contains(&format!(
            "{:x}",
            sha2::Sha256::digest(b"#!/bin/sh\nexit 0\n")
        )));
        assert!(output.contains("Setup cancelled"));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::{collections::BTreeMap, path::PathBuf};
    struct ExistingTools;
    impl Executor for ExistingTools {
        fn installed(&mut self, _: &str, _: &Environment) -> bool {
            true
        }
        fn execute(&mut self, _: &dependencies::Operation, _: &Environment) -> Result<()> {
            Err("No dependency installation is permitted in this fixture".into())
        }
    }
    fn environment(home: &Path) -> Environment {
        Environment {
            os: "windows".into(),
            architecture: "x86_64".into(),
            home: home.into(),
            search: vec![],
            binaries: BTreeMap::from([("herdr".into(), home.join("herdr.exe"))]),
            root: false,
        }
    }
    #[test]
    fn fresh_and_imported_wizard_configuration_remain_private_and_require_approval() {
        let home: PathBuf = std::env::temp_dir().join(format!(
            "daedalus-wizard-win-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::platform::ensure_private_dir(&home).unwrap();
        let source = home.join("fresh");
        let wizard = Wizard::fresh(
            &source,
            &home,
            environment(&home),
            Selection {
                tmux: false,
                herdr: true,
                harnesses: vec![],
            },
        )
        .unwrap();
        assert!(!source.exists());
        assert!(wizard.apply("unreviewed", &mut ExistingTools).is_err());
        assert!(!source.exists());
        let wizard = Wizard::fresh(
            &source,
            &home,
            environment(&home),
            Selection {
                tmux: false,
                herdr: true,
                harnesses: vec![],
            },
        )
        .unwrap();
        let approval = wizard.approval_token().to_owned();
        assert!(wizard.apply(&approval, &mut ExistingTools).unwrap().ready);
        crate::platform::validate_private_dir(&source).unwrap();
        let destination = home.join("imported");
        let wizard = Wizard::import_settings(
            &destination,
            &source.join("control.json"),
            environment(&home),
            false,
            true,
        )
        .unwrap();
        assert!(!destination.exists());
        let approval = wizard.approval_token().to_owned();
        assert!(wizard.apply(&approval, &mut ExistingTools).unwrap().ready);
        crate::platform::validate_private_dir(&destination).unwrap();
        assert!(source.join("control.json").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
