//! Windows-specific public API coverage. Cross-compilation checks this fixture;
//! it does not establish that ConPTY lifecycle works on a Windows host.
#![cfg(windows)]
use daedalus_tui::app::config::ControlConfig;
use daedalus_tui::app::native_terminal::{self, StartOptions};
use daedalus_tui::app::setup::SetupPlan;
use daedalus_tui::app::startup::{self, Launch};
use daedalus_tui::platform;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[test]
fn windows_launch_preflight_rejects_bad_dimensions_before_touching_user_state() {
    // The nonexistent CWD and invalid dimensions make ordering observable:
    // malformed requests must fail before filesystem access or daemon launch.
    let error = native_terminal::start(StartOptions {
        name: "Fixture".into(),
        workspace: "Project".into(),
        cwd: std::env::temp_dir().join("daedalus-missing-preflight-fixture"),
        argv: vec!["cmd.exe".into(), "/D".into()],
        environment: Some(BTreeMap::from([("COMSPEC".into(), "cmd.exe".into())])),
        cols: 0,
        rows: 24,
    })
    .unwrap_err();
    assert!(error.starts_with("Terminal size must be"));
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let mut nonce = [0u8; 8];
        platform::random_bytes(&mut nonce).unwrap();
        let hex: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!(
            "daedalus-win-startup-{label}-{}-{hex}",
            std::process::id()
        ));
        platform::ensure_private_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn write_private(path: &Path, bytes: &[u8]) {
    platform::ensure_private_dir(path.parent().unwrap()).unwrap();
    let mut file = platform::create_private_new(path).unwrap();
    file.write_all(bytes).unwrap();
}
fn read_profile(path: &Path) -> String {
    let mut file = platform::open_native_profile(path, false, false).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    text
}

#[test]
fn startup_rebinds_temporary_hook_commands_and_does_not_touch_telegram() {
    let scratch = Scratch::new("hooks");
    let config = scratch.0.join("config");
    let profile = scratch.0.join("profile").join("settings.json");
    let installed = scratch.0.join("Daedalus").join("bin").join("daedalus.exe");
    platform::ensure_private_dir(&config).unwrap();
    platform::ensure_private_dir(installed.parent().unwrap()).unwrap();
    write_private(&installed, b"installed-exe");
    let original = r#"{"command":"C:\\Users\\Public\\AppData\\Local\\Temp\\nsA1B2.tmp\\daedalus.exe","args":["hook","run","--registration","C:\\Users\\Public\\hooks.json"]}"#;
    write_private(&profile, original.as_bytes());
    let backup = config.join("hook-backups").join("fixture.json");
    let receipt = serde_json::json!({
        "profile": &profile,
        "backup": &backup,
        "after_sha": "fixture",
        "registration": config.join("hooks").join("fixture.json"),
    });
    write_private(
        &config.join("hook-backups").join("fixture.receipt.json"),
        &serde_json::to_vec(&receipt).unwrap(),
    );
    let changed = startup::rebind_temporary_hook_commands(&config, &installed).unwrap();
    assert_eq!(changed, 1);
    let text = read_profile(&profile);
    assert!(text.contains(&installed.display().to_string().replace('\\', "\\\\")));
    assert!(!text.contains("nsA1B2.tmp"));
    assert!(text.contains(r"C:\\Users\\Public\\hooks.json"));
    assert!(config.join("hook-reloads").read_dir().unwrap().next().is_some());
    assert_eq!(
        startup::rebind_temporary_hook_commands(&config, &installed).unwrap(),
        0
    );
    for name in [
        "telegram-service.json",
        "telegram-runtime.json",
        "telegram-stop.json",
        "telegram-process.json",
    ] {
        assert!(!config.join(name).exists(), "{name}");
    }
    let outside = serde_json::json!({
        "profile": &profile,
        "backup": config.join("hook-backups").join("other.json"),
        "after_sha": "fixture",
        "registration": scratch.0.join("other").join("hooks").join("fixture.json"),
    });
    write_private(
        &config.join("hook-backups").join("outside.receipt.json"),
        &serde_json::to_vec(&outside).unwrap(),
    );
    let error = startup::rebind_temporary_hook_commands(&config, &installed).unwrap_err();
    assert!(error.contains("outside"));
    assert!(startup::rebind_temporary_hook_commands(
        &config,
        Path::new(r"C:\Users\Public\AppData\Local\Temp\nsA1B2.tmp\daedalus.exe")
    )
    .is_err());
}

#[test]
fn fresh_setup_and_backend_launch_do_not_start_telegram() {
    let scratch = Scratch::new("setup");
    let directory = scratch.0.join("fresh");
    let workspace = scratch.0.join("workspace");
    let result = SetupPlan::fresh(&directory, ControlConfig::fresh(&workspace).unwrap())
        .unwrap()
        .apply()
        .unwrap();
    assert!(result.changed);
    assert!(directory.join("control.json").is_file());
    for name in [
        "telegram-service.json",
        "telegram-runtime.json",
        "telegram-stop.json",
        "telegram-process.json",
    ] {
        assert!(!directory.join(name).exists(), "{name}");
        assert!(!scratch.0.join(name).exists(), "{name}");
    }
    let launch = Launch::resolve(
        &["--config-dir".into(), directory.display().to_string()],
        &scratch.0,
        |_| None,
    )
    .unwrap();
    let command = launch.command(std::ffi::OsStr::new(
        r"C:\Users\Public\AppData\Local\Temp\nsA1B2.tmp\daedalus.exe",
    ));
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args.contains(&"backend".to_string()));
    assert!(args.iter().all(|arg| arg != "telegram" && arg != "serve"));
    assert!(!directory.join("telegram-runtime.json").exists());
    if let Ok(installed) = daedalus_tui::app::windows_install::destination()
        && installed.is_file()
    {
        assert_eq!(command.get_program(), installed.as_os_str());
    }
}
