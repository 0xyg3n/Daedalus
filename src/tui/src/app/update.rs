//! Public release checks and consent-bound native updates. No account credentials,
//! shell commands, executable probes, bot restarts or private configuration writes.
use reqwest::{Url, blocking::Client};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const API: &str = "https://api.github.com/repos/0xyg3n/Daedalus/releases?per_page=100";
const RELEASES: &str = "https://github.com/0xyg3n/Daedalus/releases";
const LIMIT: u64 = 4 * 1024 * 1024;
const BINARY_LIMIT: u64 = 256 * 1024 * 1024;
pub const HELP: &str = "daedalus update check [--json]\ndaedalus update install --version VERSION --approve SHA [--json]\n\nCheck compatible public releases. Beta installations include prereleases. Installation requires the exact approval SHA from check; old binaries are retained.\n";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdateStatus {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: String,
    pub message: String,
    pub review: Option<UpdateReview>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdateReview {
    pub version: String,
    pub platform: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sha256: String,
    pub size: u64,
    pub install_path: String,
    pub current_sha256: String,
    pub approval_sha256: String,
}
#[derive(Debug, Serialize)]
pub struct InstallReceipt {
    pub version: String,
    pub installed_path: String,
    pub previous_path: String,
    pub restart_required: bool,
    pub activated: bool,
    pub installer_ready: bool,
    pub message: String,
}
#[derive(Clone, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}
#[derive(Clone, Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(value: &str) -> Result<&str, String> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Release has no valid SHA-256 digest".into());
    }
    Ok(value)
}
fn platform() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-linux"),
        ("linux", "aarch64") => Ok("aarch64-linux"),
        ("windows", "x86_64") => Ok("x86_64-windows"),
        _ => Err("No reviewed native update asset for this platform".into()),
    }
}
fn asset_name(version: &Version, platform: &str) -> String {
    if platform == "x86_64-windows" {
        return format!("Daedalus-{version}-setup.exe");
    }
    format!(
        "daedalus-{version}-{platform}{}",
        if platform.ends_with("windows") {
            ".exe"
        } else {
            ""
        }
    )
}
fn trusted(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && match url.host_str() {
            Some("api.github.com") => url.path().starts_with("/repos/0xyg3n/Daedalus/releases"),
            Some("github.com") => url
                .path()
                .starts_with("/0xyg3n/Daedalus/releases/download/"),
            Some("release-assets.githubusercontent.com") => true,
            _ => false,
        }
}
fn client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && trusted(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("daedalus/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Cannot initialize release transport".into())
}
fn download(client: &Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let parsed = Url::parse(url).map_err(|_| "Invalid release URL")?;
    if !trusted(&parsed) {
        return Err("Release URL is outside trusted GitHub hosts".into());
    }
    fetch(client, url, limit)
}
fn fetch(client: &Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(if limit <= LIMIT { 10 } else { 90 }))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send()
        .map_err(|_| "Cannot reach GitHub; try again later")?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "Release request returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err("Release response exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Incomplete release response")?;
    if bytes.len() as u64 > limit {
        return Err("Release response exceeds size limit".into());
    }
    Ok(bytes)
}
fn select(
    body: &[u8],
    current: &Version,
    platform: &str,
) -> Result<Option<(Release, Version, Asset, Asset)>, String> {
    let releases: Vec<Release> =
        serde_json::from_slice(body).map_err(|_| "Invalid GitHub releases response")?;
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft {
            continue;
        }
        let Ok(version) = Version::parse(
            release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name),
        ) else {
            continue;
        };
        if current.pre.is_empty() && (release.prerelease || !version.pre.is_empty()) {
            continue;
        }
        let name = asset_name(&version, platform);
        let assets: Vec<_> = release
            .assets
            .iter()
            .filter(|a| a.name == name)
            .cloned()
            .collect();
        let checks: Vec<_> = release
            .assets
            .iter()
            .filter(|a| a.name == format!("{name}.sha256"))
            .cloned()
            .collect();
        if assets.len() != 1 || checks.len() != 1 {
            continue;
        }
        let asset = assets[0].clone();
        let checksum = checks[0].clone();
        if asset.size == 0
            || asset.size > BINARY_LIMIT
            || checksum.size == 0
            || checksum.size > 4096
        {
            continue;
        }
        let base = format!("{RELEASES}/download/{}/", release.tag_name);
        if asset.browser_download_url != format!("{base}{name}")
            || checksum.browser_download_url != format!("{base}{name}.sha256")
        {
            continue;
        }
        if asset
            .digest
            .as_deref()
            .and_then(|v| digest(v).ok())
            .is_none()
        {
            continue;
        }
        candidates.push((release, version, asset, checksum));
    }
    candidates.sort_by(|a, b| a.1.cmp_precedence(&b.1));
    Ok(candidates.pop())
}
fn checksum(bytes: &[u8], asset: &Asset) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid release checksum file")?;
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() != 1 {
        return Err("Ambiguous release checksum file".into());
    }
    let fields: Vec<_> = lines[0].split_whitespace().collect();
    if fields.len() != 2 || fields[1].trim_start_matches('*') != asset.name {
        return Err("Checksum names a different release asset".into());
    }
    let expected = digest(fields[0])?;
    if asset.digest.as_deref().and_then(|v| digest(v).ok()) != Some(expected) {
        return Err("Release checksum and GitHub digest disagree".into());
    }
    Ok(expected.into())
}
fn approval(review: &UpdateReview) -> Result<String, String> {
    let mut review = review.clone();
    review.approval_sha256.clear();
    Ok(hash(
        &serde_json::to_vec(&review).map_err(|_| "Invalid release review")?,
    ))
}
fn checked_file(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "Cannot read installed executable")?;
        let meta = file
            .metadata()
            .map_err(|_| "Cannot inspect installed executable")?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o022 != 0
            || meta.nlink() != 1
        {
            return Err(
                "Executable must be an owned regular file without shared write access or hardlinks"
                    .into(),
            );
        }
        file
    };
    #[cfg(windows)]
    let file = crate::platform::open_native_profile(path, false, false)
        .map_err(|_| "Unsafe stored release permissions")?;
    let mut bytes = Vec::new();
    file.take(BINARY_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read executable")?;
    if bytes.len() as u64 > BINARY_LIMIT {
        return Err("Executable exceeds update size limit".into());
    }
    Ok(bytes)
}
fn directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for ancestor in path.ancestors() {
            let meta =
                fs::symlink_metadata(ancestor).map_err(|_| "Cannot inspect update directory")?;
            if !meta.is_dir() || meta.mode() & 0o002 != 0 && meta.mode() & 0o1000 == 0 {
                return Err("Unsafe update directory ancestry".into());
            }
        }
        let meta = fs::metadata(path).map_err(|_| "Cannot inspect update directory")?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return Err("Update directory must be owned and not writable by others".into());
        }
    }
    #[cfg(windows)]
    crate::platform::validate_native_profile_dir(path)
        .map_err(|_| "Unsafe private update directory permissions")?;
    Ok(())
}
fn target() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|_| "Cannot identify running executable")?;
    if !executable.is_absolute() {
        return Err("Running executable path is not absolute".into());
    }
    #[cfg(unix)]
    if let Some(store) = executable.parent().and_then(Path::parent)
        && store
            .file_name()
            .is_some_and(|name| name == ".daedalus-updates")
        && let Some(parent) = store.parent()
    {
        for name in ["daedalus", "daedalus-tui"] {
            let link = parent.join(name);
            if fs::read_link(&link).ok().as_deref() == Some(executable.as_path()) {
                return Ok(link);
            }
        }
        return Err(
            "This retained release is not the active command; update through the installed command"
                .into(),
        );
    }
    // The native Linux installer uses prefix/share/daedalus/native/HASH/daedalus.
    #[cfg(unix)]
    if let Some(native) = executable.parent().and_then(Path::parent)
        && native.file_name().is_some_and(|name| name == "native")
        && let Some(prefix) = native
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
    {
        let link = prefix.join("bin/daedalus");
        if fs::read_link(&link).ok().as_deref() == Some(executable.as_path()) {
            return Ok(link);
        }
    }
    Ok(executable)
}
fn target_bytes(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        let file = crate::platform::open_regular(path)
            .map_err(|_| "Cannot read the running Daedalus executable")?;
        let mut bytes = Vec::new();
        file.take(BINARY_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read the running Daedalus executable")?;
        if bytes.len() as u64 > BINARY_LIMIT {
            return Err("Running executable exceeds update size limit".into());
        }
        return Ok(bytes);
    }
    #[cfg(unix)]
    {
        directory(path.parent().ok_or("Missing executable directory")?)?;
        let meta = fs::symlink_metadata(path).map_err(|_| "Installed executable disappeared")?;
        if meta.file_type().is_symlink() {
            let destination = fs::read_link(path).map_err(|_| "Cannot inspect executable link")?;
            if !destination.is_absolute() {
                return Err("Executable link must have an absolute target".into());
            }
            directory(
                destination
                    .parent()
                    .ok_or("Missing executable target directory")?,
            )?;
            checked_file(&destination)
        } else {
            checked_file(path)
        }
    }
}
fn review_at(client: &Client, current: &str, executable: &Path) -> Result<UpdateStatus, String> {
    let installed = Version::parse(current)
        .map_err(|_| "Installed version is not semantic versioning compatible")?;
    let platform = platform()?;
    let body = download(client, API, LIMIT)?;
    let Some((release, version, asset, sum)) = select(&body, &installed, platform)? else {
        return Ok(UpdateStatus {
            status: "no_release".into(),
            current_version: current.into(),
            latest_version: None,
            release_url: RELEASES.into(),
            message:
                "No compatible published release with a verified binary and checksum was found."
                    .into(),
            review: None,
        });
    };
    let status = match version.cmp_precedence(&installed) {
        std::cmp::Ordering::Greater => "update_available",
        std::cmp::Ordering::Equal => "up_to_date",
        std::cmp::Ordering::Less => "ahead",
    };
    let mut result = UpdateStatus {
        status: status.into(),
        current_version: current.into(),
        latest_version: Some(version.to_string()),
        release_url: format!("{RELEASES}/tag/{}", release.tag_name),
        message: format!("Installed {installed}; compatible release {version}."),
        review: None,
    };
    if status == "update_available" {
        let bytes = download(client, &sum.browser_download_url, 4096)?;
        if bytes.len() as u64 != sum.size {
            return Err("Release checksum size changed".into());
        }
        if let Some(expected) = &sum.digest
            && hash(&bytes) != digest(expected)?
        {
            return Err("Checksum file digest mismatch".into());
        }
        let mut review = UpdateReview {
            version: version.to_string(),
            platform: platform.into(),
            asset_name: asset.name.clone(),
            asset_url: asset.browser_download_url.clone(),
            sha256: checksum(&bytes, &asset)?,
            size: asset.size,
            install_path: executable.to_string_lossy().into_owned(),
            current_sha256: hash(&target_bytes(executable)?),
            approval_sha256: String::new(),
        };
        review.approval_sha256 = approval(&review)?;
        result.review = Some(review);
        result.message = format!(
            "Daedalus {version} is available. Review and approve installation; existing sessions will not restart."
        );
    }
    Ok(result)
}
pub fn check() -> Result<UpdateStatus, String> {
    review_at(&client()?, env!("CARGO_PKG_VERSION"), &target()?)
}
fn executable_format(bytes: &[u8], platform: &str) -> Result<(), String> {
    let valid = if platform.ends_with("linux") {
        bytes.len() >= 20
            && bytes[..6] == [0x7f, b'E', b'L', b'F', 2, 1]
            && u16::from_le_bytes([bytes[18], bytes[19]])
                == if platform.starts_with("aarch64") {
                    183
                } else {
                    62
                }
    } else if platform == "x86_64-windows" {
        bytes.get(..2) == Some(b"MZ")
            && bytes.get(60..64).is_some_and(|value| {
                let offset = u32::from_le_bytes(value.try_into().unwrap()) as usize;
                bytes.get(offset..offset.saturating_add(6)) == Some(&[b'P', b'E', 0, 0, 0x4c, 0x01])
            })
    } else {
        false
    };
    if valid {
        Ok(())
    } else {
        Err("Verified asset is not a matching native executable".into())
    }
}
fn store_file(root: &Path, bytes: &[u8], executable: bool) -> Result<PathBuf, String> {
    let directory = root.join(hash(bytes));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if !directory.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .map_err(|_| "Cannot create release directory")?;
        }
    }
    #[cfg(windows)]
    crate::platform::ensure_private_dir(&directory)
        .map_err(|_| "Cannot create release directory")?;
    self::directory(&directory)?;
    let path = directory.join(if cfg!(windows) {
        "daedalus.exe"
    } else {
        "daedalus"
    });
    if path.exists() {
        if checked_file(&path)? != bytes {
            return Err("Stored release content changed".into());
        }
        return Ok(path);
    }
    crate::core::layouts::write_private_limit(&path, bytes, BINARY_LIMIT)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot enable verified executable")?;
    }
    #[cfg(windows)]
    let _ = executable;
    Ok(path)
}
fn update_lock(root: &Path) -> Result<fs::File, String> {
    let path = root.join("update.lock");
    #[cfg(unix)]
    {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "Cannot open update lock")?;
        let meta = file.metadata().map_err(|_| "Cannot inspect update lock")?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
        {
            return Err("Unsafe update lock".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another update is in progress".into());
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        let file = crate::platform::open_private(&path, true, true)
            .map_err(|_| "Cannot open update lock")?;
        crate::platform::lock_exclusive(&file, true)
            .map_err(|_| "Another update is in progress")?;
        Ok(file)
    }
}
fn activate(review: &UpdateReview, bytes: &[u8]) -> Result<InstallReceipt, String> {
    if bytes.len() as u64 != review.size || hash(bytes) != review.sha256 {
        return Err("Downloaded release size or digest mismatch".into());
    }
    executable_format(bytes, &review.platform)?;
    let target = Path::new(&review.install_path);
    let old = target_bytes(target)?;
    if hash(&old) != review.current_sha256 {
        return Err("Installed executable changed after review; check again".into());
    }
    #[cfg(windows)]
    return stage_windows(review, bytes, &old, target);
    #[cfg(unix)]
    {
        let parent = target.parent().ok_or("Missing executable directory")?;
        let root = parent.join(".daedalus-updates");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if !root.exists() {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&root)
                    .map_err(|_| "Cannot create private update directory")?;
            }
        }
        #[cfg(windows)]
        crate::platform::ensure_private_dir(&root)
            .map_err(|_| "Cannot create private update directory")?;
        directory(&root)?;
        let _lock = update_lock(&root)?;
        if hash(&target_bytes(target)?) != review.current_sha256 {
            return Err("Installed executable changed while acquiring update lock".into());
        }
        let previous = store_file(&root, &old, true)?;
        let installed = store_file(&root, bytes, true)?;
        // Revalidate immediately before switching; preserve both content-addressed binaries.
        if hash(&target_bytes(target)?) != review.current_sha256 {
            return Err("Installed executable changed during installation".into());
        }
        let mut random = [0; 16];
        crate::platform::random_bytes(&mut random).map_err(|_| "Cannot create update nonce")?;
        let temporary = parent.join(format!(".daedalus-update-{}", hash(&random)));
        std::os::unix::fs::symlink(&installed, &temporary)
            .map_err(|_| "Cannot stage executable pointer")?;
        if let Err(error) = fs::rename(&temporary, target) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot activate verified release: {error}"));
        }
        crate::platform::sync_directory(parent).map_err(
            |_| "Release activated but directory synchronization failed; inspect installation",
        )?;
        Ok(InstallReceipt {
            version: review.version.clone(),
            installed_path: installed.display().to_string(),
            previous_path: previous.display().to_string(),
            restart_required: true,
            activated: true,
            installer_ready: false,
            message: "Update installed. Restart Daedalus when ready. Existing sessions and services were not restarted; the previous executable is retained.".into(),
        })
    }
}

#[cfg(windows)]
fn stage_windows(
    review: &UpdateReview,
    bytes: &[u8],
    old: &[u8],
    target: &Path,
) -> Result<InstallReceipt, String> {
    let base = crate::platform::user_local_data_directory()
        .map_err(|_| "Windows local data directory unavailable")?
        .join("Daedalus");
    crate::platform::secure_owned_dir(&base)
        .map_err(|_| "Cannot secure Daedalus update directory")?;
    let root = base.join(".daedalus-updates");
    crate::platform::ensure_private_dir(&root)
        .map_err(|_| "Cannot create private update directory")?;
    let _lock = update_lock(&root)?;
    if hash(&target_bytes(target)?) != review.current_sha256 {
        return Err("Running executable changed during update".into());
    }
    let previous = store_file(&root, old, true)?;
    let package_dir = root.join(hash(bytes));
    crate::platform::ensure_private_dir(&package_dir)
        .map_err(|_| "Cannot create private installer directory")?;
    let installer = package_dir.join("Daedalus-setup.exe");
    if installer.exists() {
        if checked_file(&installer)? != bytes {
            return Err("Stored installer content changed".into());
        }
    } else {
        crate::core::layouts::write_private_limit(&installer, bytes, BINARY_LIMIT)?;
    }
    if hash(&target_bytes(target)?) != review.current_sha256 {
        return Err("Running executable changed while staging update".into());
    }
    Ok(InstallReceipt {
        version: review.version.clone(),
        installed_path: installer.display().to_string(),
        previous_path: previous.display().to_string(),
        restart_required: true,
        activated: false,
        installer_ready: true,
        message: "Verified installer ready. The TUI closes and runs it; command-line users can run the returned installer path after closing Daedalus. Existing terminals stay open.".into(),
    })
}
pub fn install(version: &str, approved: &str) -> Result<InstallReceipt, String> {
    digest(approved)?;
    let client = client()?;
    let status = review_at(&client, env!("CARGO_PKG_VERSION"), &target()?)?;
    let review = status.review.ok_or("No installable newer release")?;
    if version != review.version || approved != review.approval_sha256 {
        return Err(
            "Release review changed or approval does not match; run update check again".into(),
        );
    }
    let bytes = download(&client, &review.asset_url, BINARY_LIMIT)?;
    activate(&review, &bytes)
}
pub fn run(args: &[String], output: &mut impl Write) -> Result<(), String> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return write!(output, "{HELP}").map_err(|_| "Cannot write update help".into());
    }
    if args == ["check"] || args == ["check", "--json"] {
        let result = check()?;
        if args.len() == 2 {
            serde_json::to_writer_pretty(&mut *output, &result)
                .map_err(|_| "Cannot write release check")?;
        } else {
            write!(output, "{}\n{}", result.message, result.release_url)
                .map_err(|_| "Cannot write release check")?;
            if let Some(review) = result.review {
                write!(
                    output,
                    "\nInstall: daedalus update install --version {} --approve {}",
                    review.version, review.approval_sha256
                )
                .map_err(|_| "Cannot write release review")?;
            }
        }
    } else if (args.len() == 5 || args.len() == 6 && args[5] == "--json")
        && args[0] == "install"
        && args[1] == "--version"
        && args[3] == "--approve"
    {
        let result = install(&args[2], &args[4])?;
        if args.len() == 6 {
            serde_json::to_writer_pretty(&mut *output, &result)
                .map_err(|_| "Cannot write install receipt")?;
        } else {
            write!(output, "{}", result.message).map_err(|_| "Cannot write install receipt")?;
        }
    } else {
        return Err(HELP.into());
    }
    writeln!(output).map_err(|_| "Cannot write update result".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        bytes[18] = 62;
        bytes
    }
    fn windows_installer() -> Vec<u8> {
        let mut bytes = vec![0; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..70].copy_from_slice(&[b'P', b'E', 0, 0, 0x4c, 0x01]);
        bytes
    }
    fn release(version: &str, prerelease: bool) -> serde_json::Value {
        let name = format!("daedalus-{version}-x86_64-linux");
        let url = format!("{RELEASES}/download/v{version}/{name}");
        json!({"tag_name":format!("v{version}"),"draft":false,"prerelease":prerelease,"assets":[{"name":name,"size":64,"browser_download_url":url,"digest":format!("sha256:{}",hash(&elf()))},{"name":format!("{name}.sha256"),"size":100,"browser_download_url":format!("{url}.sha256"),"digest":null}]})
    }
    #[test]
    fn selects_compatible_semver_and_beta_channel() {
        let bytes = serde_json::to_vec(&json!([
            release("0.9.0", false),
            release("0.10.0", false),
            release("0.11.0-beta.1", true)
        ]))
        .unwrap();
        assert_eq!(
            select(&bytes, &Version::parse("0.9.0").unwrap(), "x86_64-linux")
                .unwrap()
                .unwrap()
                .1
                .to_string(),
            "0.10.0"
        );
        assert_eq!(
            select(
                &bytes,
                &Version::parse("0.9.0-beta.1").unwrap(),
                "x86_64-linux"
            )
            .unwrap()
            .unwrap()
            .1
            .to_string(),
            "0.11.0-beta.1"
        );
        assert!(
            select(&bytes, &Version::parse("0.9.0").unwrap(), "aarch64-linux")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            Version::parse("1.0.0+one")
                .unwrap()
                .cmp_precedence(&Version::parse("1.0.0+two").unwrap()),
            std::cmp::Ordering::Equal
        );
    }
    #[test]
    fn windows_update_selects_verified_setup_package() {
        let version = Version::parse("0.2.0-beta.12").unwrap();
        let name = asset_name(&version, "x86_64-windows");
        assert_eq!(name, "Daedalus-0.2.0-beta.12-setup.exe");
        let url = format!("{RELEASES}/download/v{version}/{name}");
        let release = json!([{"tag_name":format!("v{version}"),"draft":false,"prerelease":true,"assets":[
            {"name":name,"size":128,"browser_download_url":url,"digest":format!("sha256:{}",hash(&windows_installer()))},
            {"name":format!("{name}.sha256"),"size":100,"browser_download_url":format!("{url}.sha256"),"digest":null}
        ]}]);
        let selected = select(
            &serde_json::to_vec(&release).unwrap(),
            &Version::parse("0.2.0-beta.11").unwrap(),
            "x86_64-windows",
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.2.name, name);
        assert!(executable_format(&windows_installer(), "x86_64-windows").is_ok());
        assert!(executable_format(&elf(), "x86_64-windows").is_err());
    }
    #[test]
    fn rejects_untrusted_ambiguous_and_undigested_assets() {
        for variant in 0..4 {
            let mut candidate = release("1.0.0", false);
            match variant {
                0 => {
                    candidate["assets"][0]["browser_download_url"] =
                        "https://evil.invalid/binary".into()
                }
                1 => candidate["assets"][0]["digest"] = serde_json::Value::Null,
                2 => candidate["draft"] = true.into(),
                _ => {
                    let asset = candidate["assets"][0].clone();
                    candidate["assets"].as_array_mut().unwrap().push(asset);
                }
            }
            assert!(
                select(
                    &serde_json::to_vec(&vec![candidate]).unwrap(),
                    &Version::parse("0.1.0").unwrap(),
                    "x86_64-linux"
                )
                .unwrap()
                .is_none()
            );
        }
        for url in [
            "http://github.com/0xyg3n/Daedalus/releases/download/a/b",
            "https://github.com.evil.invalid/",
            "https://user@github.com/0xyg3n/Daedalus/releases/download/a/b",
            "https://github.com/other/repo/releases/download/a/b",
            "https://release-assets.githubusercontent.com:444/a",
        ] {
            assert!(!trusted(&Url::parse(url).unwrap()));
        }
    }
    #[test]
    fn checksum_and_executable_identity_are_both_required() {
        let candidate: Release = serde_json::from_value(release("1.0.0", false)).unwrap();
        let asset = &candidate.assets[0];
        let sum = format!("{}  {}\n", hash(&elf()), asset.name);
        assert_eq!(checksum(sum.as_bytes(), asset).unwrap(), hash(&elf()));
        assert!(checksum(format!("{}  other\n", hash(&elf())).as_bytes(), asset).is_err());
        assert!(
            checksum(
                format!("{}  {}\n", "0".repeat(64), asset.name).as_bytes(),
                asset
            )
            .is_err()
        );
        assert!(checksum(format!("{sum}{sum}").as_bytes(), asset).is_err());
        assert!(executable_format(&elf(), "x86_64-linux").is_ok());
        assert!(executable_format(&elf(), "aarch64-linux").is_err());
        assert!(executable_format(b"#!/bin/sh\n", "x86_64-linux").is_err());
    }
    #[test]
    fn http_fixture_is_bounded_and_has_no_credentials() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody"
            )
            .unwrap();
        });
        assert!(fetch(&client().unwrap(), &endpoint, 3).is_err());
        server.join().unwrap();
        assert!(download(&client().unwrap(), &endpoint, 100).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn install_is_digest_bound_atomic_and_keeps_rollback_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 16];
        crate::platform::random_bytes(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("daedalus-update-test-{}", hash(&nonce)));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.join("daedalus");
        let old = b"old fixture";
        fs::write(&target, old).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = elf();
        let mut review = UpdateReview {
            version: "1.0.0".into(),
            platform: "x86_64-linux".into(),
            asset_name: "asset".into(),
            asset_url: "https://invalid".into(),
            sha256: hash(&bytes),
            size: bytes.len() as u64,
            install_path: target.display().to_string(),
            current_sha256: hash(old),
            approval_sha256: String::new(),
        };
        review.approval_sha256 = approval(&review).unwrap();
        let original_approval = review.approval_sha256.clone();
        review.size += 1;
        assert_ne!(approval(&review).unwrap(), original_approval);
        assert!(activate(&review, &bytes).is_err());
        assert_eq!(fs::read(&target).unwrap(), old);
        review.size -= 1;
        let receipt = activate(&review, &bytes).unwrap();
        assert!(receipt.activated);
        assert!(receipt.restart_required);
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::read(&receipt.previous_path).unwrap(), old);
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(activate(&review, &bytes).is_err());
        let rootstore = root.join(".daedalus-updates");
        let _lock = update_lock(&rootstore).unwrap();
        assert!(update_lock(&rootstore).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
