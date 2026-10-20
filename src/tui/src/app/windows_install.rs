//! Per-user Windows installation from the already running executable.
//! The release installer calls this command, so users do not need PowerShell.
use crate::{core::layouts, platform};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_BINARY: u64 = 256 * 1024 * 1024;

fn receipt_record(bytes: &[u8]) -> Result<serde_json::Value, String> {
    // Windows PowerShell 5.1 installers previously wrote UTF-8 with a BOM.
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
        .map_err(|_| "Invalid Daedalus installer receipt".into())
}

fn private_dir(path: &Path) -> Result<(), String> {
    platform::secure_owned_dir(path).map_err(|e| format!("Cannot secure install directory: {e}"))
}

pub fn destination() -> Result<PathBuf, String> {
    let base = platform::user_local_data_directory().map_err(|e| e.to_string())?;
    Ok(base.join("Daedalus/bin/daedalus.exe"))
}

/// Run from the uninstaller's temporary helper, never the file being removed.
/// Only executable installation files are removed by NSIS; private data stays.
pub fn prepare_uninstall() -> Result<(), String> {
    let destination = destination()?;
    let cwd = std::env::current_dir().map_err(|_| "Cannot resolve configuration directory")?;
    let preferences = super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
    prepare_uninstall_at(
        &destination,
        &preferences,
        std::time::Duration::from_secs(150),
    )
}

fn prepare_uninstall_at(
    destination: &Path,
    preferences: &Path,
    timeout: std::time::Duration,
) -> Result<(), String> {
    if !destination.exists() {
        return Ok(());
    }
    verify_installed(destination)?;
    let deadline = std::time::Instant::now() + timeout;
    for directory in super::instances::saved_directories(preferences)? {
        super::telegram_supervisor::stop_for_uninstall(
            &directory,
            destination,
            deadline.saturating_duration_since(std::time::Instant::now()),
        )?;
    }
    // Check that the image can actually be removed before NSIS deletes anything.
    // Never force-stop unrelated TUI, native terminal or agent processes.
    platform::check_executable_not_running(destination)
        .map_err(|_| "Close other Daedalus windows before uninstalling. Telegram was stopped gracefully; your configuration is preserved.".into())
}

fn verify_installed(destination: &Path) -> Result<(), String> {
    let base = destination
        .parent()
        .and_then(Path::parent)
        .ok_or("Missing Daedalus install directory")?;
    let receipt = base.join("install.json");
    platform::secure_owned_file(&receipt).map_err(|_| "Cannot verify install receipt ownership")?;
    let bytes = layouts::read_private(&receipt)?
        .ok_or("Existing Daedalus installer receipt disappeared")?;
    let record = receipt_record(&bytes)?;
    let expected = record
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or("Invalid Daedalus installer receipt checksum")?;
    if record.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("Unsupported Daedalus installer receipt".into());
    }
    let mut installed = platform::open_regular(destination).map_err(|e| e.to_string())?;
    let mut previous = Vec::new();
    Read::by_ref(&mut installed)
        .take(MAX_BINARY + 1)
        .read_to_end(&mut previous)
        .map_err(|e| e.to_string())?;
    if previous.len() as u64 > MAX_BINARY
        || !format!("{:x}", Sha256::digest(&previous)).eq_ignore_ascii_case(expected)
    {
        return Err(
            "Installed executable differs from its receipt; installation has been kept".into(),
        );
    }
    Ok(())
}

fn wait_for_parent(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Storage::FileSystem::SYNCHRONIZE,
        System::Threading::{OpenProcess, WaitForSingleObject},
    };
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err("Cannot wait for the previous Daedalus process".into());
    }
    let result = unsafe { WaitForSingleObject(handle, 45_000) };
    unsafe { CloseHandle(handle) };
    match result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err("Close the previous Daedalus window and retry installation".into()),
        _ => Err("Cannot confirm previous Daedalus process exited".into()),
    }
}

pub fn install_current(wait_pid: Option<u32>) -> Result<PathBuf, String> {
    if let Some(pid) = wait_pid {
        wait_for_parent(pid)?;
    }
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let destination = destination()?;
    let base = destination
        .parent()
        .and_then(Path::parent)
        .ok_or("Missing Daedalus install directory")?;
    let bin = destination
        .parent()
        .ok_or("Missing Daedalus bin directory")?;
    private_dir(base)?;
    private_dir(bin)?;

    let mut input = platform::open_regular(&source).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut input)
        .take(MAX_BINARY + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BINARY {
        return Err("Daedalus executable exceeds the install size limit".into());
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));

    let receipt = base.join("install.json");
    if destination.exists() {
        verify_installed(&destination)?;
    }
    let same_file = if destination.exists() {
        let target = platform::open_regular(&destination).map_err(|e| e.to_string())?;
        platform::file_identity(&input).map_err(|e| e.to_string())?
            == platform::file_identity(&target).map_err(|e| e.to_string())?
    } else {
        false
    };
    if destination.exists() {
        platform::secure_owned_file(&destination)
            .map_err(|e| format!("Cannot secure installed executable: {e}"))?;
    }
    if !same_file {
        let mut random = [0u8; 16];
        platform::random_bytes(&mut random).map_err(|e| e.to_string())?;
        let temporary = bin.join(format!(
            ".daedalus-install-{:x}.tmp",
            u128::from_le_bytes(random)
        ));
        let result = (|| {
            let mut output = platform::create_private_new(&temporary).map_err(|e| e.to_string())?;
            output.write_all(&bytes).map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
            drop(output);
            platform::replace_private(&temporary, &destination).map_err(|e| {
                format!("Cannot replace installed Daedalus; close the running app and retry: {e}")
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    let installed = platform::open_native_profile(&destination, false, false)
        .map_err(|e| format!("Installed executable permissions are unsafe: {e}"))?;
    let mut check = Vec::new();
    installed
        .take(MAX_BINARY + 1)
        .read_to_end(&mut check)
        .map_err(|e| e.to_string())?;
    if Sha256::digest(&check) != Sha256::digest(&bytes) {
        return Err("Installed executable checksum changed".into());
    }

    if receipt.exists() {
        platform::secure_owned_file(&receipt)
            .map_err(|e| format!("Cannot secure install receipt: {e}"))?;
    }
    let record = serde_json::json!({
        "version": 1,
        "sha256": digest,
        "target": "x86_64-windows"
    });
    layouts::write_private(
        &receipt,
        &serde_json::to_vec(&record).map_err(|e| e.to_string())?,
    )?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Fixture {
        root: PathBuf,
        executable: PathBuf,
        preferences: PathBuf,
        directory: PathBuf,
    }
    impl Fixture {
        fn new(saved: bool) -> Self {
            let mut random = [0u8; 16];
            platform::random_bytes(&mut random).unwrap();
            let root = std::env::temp_dir().join(format!(
                "daedalus-uninstall-{:x}",
                u128::from_le_bytes(random)
            ));
            let executable = root.join("install/bin/daedalus.exe");
            platform::ensure_private_dir(executable.parent().unwrap()).unwrap();
            let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
            let mut output = platform::create_private_new(&executable).unwrap();
            output.write_all(&bytes).unwrap();
            drop(output);
            layouts::write_private(&root.join("install/install.json"), &serde_json::to_vec(&serde_json::json!({"version":1,"sha256":format!("{:x}",Sha256::digest(&bytes))})).unwrap()).unwrap();
            let preferences = root.join("config");
            let directory = if saved {
                root.join("custom-instance")
            } else {
                preferences.clone()
            };
            layouts::write_private(
                &preferences.join("dashboard.json"),
                &serde_json::to_vec(&serde_json::json!({"instances":[directory]})).unwrap(),
            )
            .unwrap();
            let config = super::super::telegram_config::ServiceConfig {
                version: 1,
                instance: "fixture".into(),
                bot_id: 123,
                token_file: directory.join("token"),
                grants: vec![],
                listen_chats: Default::default(),
                registrations: vec![],
                hook_bindings: vec![],
            };
            layouts::write_private(
                &directory.join("telegram-service.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            // No real token, network client or provider is used by this child.
            layouts::write_private(&directory.join("preserve-private-data"), b"fixture").unwrap();
            Self {
                root,
                executable,
                preferences,
                directory,
            }
        }
        fn child(&self, ignores_stop: bool) -> std::process::Child {
            let child = std::process::Command::new(&self.executable)
                .args([
                    "--ignored",
                    "--exact",
                    "app::windows_install::tests::uninstall_service_child",
                    "--test-threads=1",
                ])
                .env("DAEDALUS_UNINSTALL_FIXTURE", &self.directory)
                .env(
                    "DAEDALUS_UNINSTALL_IGNORE_STOP",
                    if ignores_stop { "1" } else { "0" },
                )
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.directory.join("ready").exists() {
                assert!(Instant::now() < deadline, "Fixture service did not start");
                std::thread::sleep(Duration::from_millis(20));
            }
            child
        }
        fn assert_preserved(&self) {
            assert!(self.executable.exists());
            assert!(self.root.join("install/install.json").exists());
            assert_eq!(
                fs::read(self.directory.join("preserve-private-data")).unwrap(),
                b"fixture"
            );
            assert!(self.directory.join("telegram-service.json").exists());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    #[ignore = "Child process fixture; invoked only by the uninstall tests"]
    fn uninstall_service_child() {
        let directory = PathBuf::from(std::env::var_os("DAEDALUS_UNINSTALL_FIXTURE").unwrap());
        let config = super::super::telegram_config::ServiceConfig::load(
            &directory.join("telegram-service.json"),
        )
        .unwrap();
        let owner = super::super::telegram_service::Owner::acquire(
            &directory,
            &directory.join("owners"),
            config.bot_id,
        )
        .unwrap();
        super::super::telegram_supervisor::record(
            &directory,
            &config,
            super::super::telegram_supervisor::Phase::Running,
        )
        .unwrap();
        layouts::write_private(&directory.join("ready"), b"fixture").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if std::env::var("DAEDALUS_UNINSTALL_IGNORE_STOP").unwrap() != "1"
                && super::super::telegram_supervisor::stop_requested(&directory).unwrap()
            {
                break;
            }
            assert!(Instant::now() < deadline, "Fixture was not stopped");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(owner);
    }
    #[test]
    fn uninstall_waits_for_default_and_saved_service_exit_and_preserves_data() {
        for saved in [false, true] {
            let f = Fixture::new(saved);
            let mut child = f.child(false);
            let result =
                prepare_uninstall_at(&f.executable, &f.preferences, Duration::from_secs(10));
            if result.is_err() {
                let _ = child.kill();
            }
            let status = child.wait().unwrap();
            result.unwrap();
            assert!(status.success());
            f.assert_preserved();
        }
    }
    #[test]
    fn uninstall_timeout_keeps_installation_and_does_not_force_kill() {
        let f = Fixture::new(false);
        let mut child = f.child(true);
        let result = prepare_uninstall_at(&f.executable, &f.preferences, Duration::from_millis(50));
        let remained_running = child.try_wait().unwrap().is_none();
        // Only this isolated test owns and terminates its deliberately stuck child.
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(result.unwrap_err().contains("not finished shutting down"));
        assert!(remained_running);
        f.assert_preserved();
    }
    #[test]
    fn uninstall_rejects_receipt_tampering_before_requesting_shutdown() {
        let f = Fixture::new(false);
        layouts::write_private(&f.root.join("install/install.json"), br#"{"version":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}"#).unwrap();
        assert!(
            prepare_uninstall_at(&f.executable, &f.preferences, Duration::from_millis(50)).is_err()
        );
        assert!(!f.directory.join("telegram-stop.json").exists());
        f.assert_preserved();
    }
    #[test]
    fn old_powershell_receipts_and_native_receipts_have_the_same_fields() {
        let json = br#"{"version":1,"sha256":"fixture-checksum"}"#;
        let mut legacy = b"\xef\xbb\xbf".to_vec();
        legacy.extend_from_slice(json);
        assert_eq!(
            receipt_record(json).unwrap(),
            receipt_record(&legacy).unwrap()
        );
        assert!(receipt_record(b"\xef\xbb\xbfnot-json").is_err());
    }
}
