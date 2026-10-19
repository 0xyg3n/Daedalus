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

fn private_dir(path: &Path) -> Result<(), String> {
    platform::secure_owned_dir(path).map_err(|e| format!("Cannot secure install directory: {e}"))
}

pub fn destination() -> Result<PathBuf, String> {
    let base = platform::user_local_data_directory().map_err(|e| e.to_string())?;
    Ok(base.join("Daedalus/bin/daedalus.exe"))
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
        if !receipt.exists() {
            return Err(
                "Existing Daedalus executable has no installer receipt; refusing to replace it"
                    .into(),
            );
        }
        platform::secure_owned_file(&receipt)
            .map_err(|e| format!("Cannot secure install receipt: {e}"))?;
        let record = layouts::read_private(&receipt)?
            .ok_or("Existing Daedalus installer receipt disappeared")?;
        let record: serde_json::Value =
            serde_json::from_slice(&record).map_err(|_| "Invalid Daedalus installer receipt")?;
        let expected = record
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .filter(|value| value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or("Invalid Daedalus installer receipt checksum")?;
        if record.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err("Unsupported Daedalus installer receipt".into());
        }
        let mut installed = platform::open_regular(&destination).map_err(|e| e.to_string())?;
        let mut previous = Vec::new();
        Read::by_ref(&mut installed)
            .take(MAX_BINARY + 1)
            .read_to_end(&mut previous)
            .map_err(|e| e.to_string())?;
        if previous.len() as u64 > MAX_BINARY
            || !format!("{:x}", Sha256::digest(&previous)).eq_ignore_ascii_case(expected)
        {
            return Err(
                "Installed executable differs from its receipt; refusing to replace it".into(),
            );
        }
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
