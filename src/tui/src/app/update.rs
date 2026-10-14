//! Read-only release checks. No private configuration or credentials are sent.
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    time::Duration,
};

const API: &str = "https://api.github.com/repos/0xyg3n/Daedalus/releases/latest";
const RELEASES: &str = "https://github.com/0xyg3n/Daedalus/releases";
const LIMIT: u64 = 1024 * 1024;
pub const HELP: &str = "daedalus update check [--json]\n\nCheck the latest public stable GitHub release. No files are downloaded or installed.\n";

#[derive(Debug, Serialize)]
pub struct UpdateStatus {
    pub status: &'static str,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: String,
    pub message: String,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

pub fn check() -> Result<UpdateStatus, String> {
    check_at(API, env!("CARGO_PKG_VERSION"))
}
fn check_at(endpoint: &str, current: &str) -> Result<UpdateStatus, String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("daedalus/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Cannot initialize release check")?;
    let response = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send()
        .map_err(|_| "Release check unavailable: could not reach GitHub. Try again later.")?;
    let status = response.status().as_u16();
    if status != 200 {
        return evaluate(status, &[], current);
    }
    let mut body = Vec::new();
    response
        .take(LIMIT + 1)
        .read_to_end(&mut body)
        .map_err(|_| "Release check unavailable: incomplete GitHub response")?;
    if body.len() as u64 > LIMIT {
        return Err("Release check unavailable: response exceeds size limit".into());
    }
    evaluate(status, &body, current)
}
fn evaluate(status: u16, body: &[u8], current: &str) -> Result<UpdateStatus, String> {
    let installed = Version::parse(current)
        .map_err(|_| "Installed version is not semantic versioning compatible")?;
    if status == 404 {
        return Ok(UpdateStatus {
            status: "no_release", current_version: current.into(), latest_version: None,
            release_url: RELEASES.into(),
            message: "No public stable release was found. This does not establish whether your source build is current.".into(),
        });
    }
    if matches!(status, 403 | 429) {
        return Err("Release check unavailable: GitHub denied or rate-limited the request. Try again later.".into());
    }
    if status != 200 {
        return Err(format!(
            "Release check unavailable: GitHub returned HTTP {status}"
        ));
    }
    let release: Release = serde_json::from_slice(body)
        .map_err(|_| "Release check unavailable: invalid GitHub response")?;
    if release.draft || release.prerelease {
        return Err(
            "Release check unavailable: GitHub did not return a stable published release".into(),
        );
    }
    let latest = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(
        |_| "Latest release tag is not a semantic version; inspect GitHub releases manually",
    )?;
    if !latest.pre.is_empty() {
        return Err(
            "Latest release tag identifies a prerelease; inspect GitHub releases manually".into(),
        );
    }
    let (status, message) = match latest.cmp_precedence(&installed) {
        std::cmp::Ordering::Greater => (
            "update_available",
            format!(
                "Daedalus {latest} is available. Installed version: {installed}. Review the release before installing."
            ),
        ),
        std::cmp::Ordering::Equal => (
            "up_to_date",
            format!("Installed version {installed} matches the latest stable release."),
        ),
        std::cmp::Ordering::Less => (
            "ahead",
            format!(
                "Installed version {installed} is newer than the latest stable release {latest}."
            ),
        ),
    };
    Ok(UpdateStatus {
        status,
        current_version: current.into(),
        latest_version: Some(latest.to_string()),
        release_url: format!("{RELEASES}/tag/{}", release.tag_name),
        message,
    })
}

pub fn run(args: &[String], output: &mut impl Write) -> Result<(), String> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return write!(output, "{HELP}").map_err(|_| "Cannot write update help".into());
    }
    if args != ["check"] && args != ["check", "--json"] {
        return Err("Usage: daedalus update check [--json]".into());
    }
    let result = check()?;
    if args.len() == 2 {
        serde_json::to_writer_pretty(&mut *output, &result)
            .map_err(|_| "Cannot write release check")?;
        writeln!(output).map_err(|_| "Cannot write release check".into())
    } else {
        writeln!(output, "{}\n{}", result.message, result.release_url)
            .map_err(|_| "Cannot write release check".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"tag_name":tag,"draft":false,"prerelease":false}))
            .unwrap()
    }
    #[test]
    fn compares_semantic_precedence_not_strings_or_build_metadata() {
        for (current, latest, expected) in [
            ("0.9.0", "v0.10.0", "update_available"),
            ("1.0.0-rc.2", "1.0.0", "update_available"),
            ("1.0.0+local", "v1.0.0+release", "up_to_date"),
            ("2.0.0", "v1.9.0", "ahead"),
        ] {
            assert_eq!(
                evaluate(200, &release(latest), current).unwrap().status,
                expected
            );
        }
    }
    #[test]
    fn missing_releases_and_unusable_responses_are_not_up_to_date() {
        assert_eq!(evaluate(404, b"", "0.1.0").unwrap().status, "no_release");
        for status in [301, 403, 429, 500] {
            assert!(evaluate(status, b"", "0.1.0").is_err());
        }
        for body in [
            b"{}".to_vec(),
            release("nightly"),
            release("v1.0.0-rc.1"),
            release("v1.0.0\x1b"),
        ] {
            assert!(evaluate(200, &body, "0.1.0").is_err());
        }
        assert!(
            evaluate(
                200,
                br#"{"tag_name":"v1.0.0","draft":true,"prerelease":false}"#,
                "0.1.0"
            )
            .is_err()
        );
    }
    #[test]
    fn isolated_http_check_sends_no_credentials() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
            assert!(request.starts_with("get /latest http/1.1"));
            assert!(!request.contains("authorization:"));
            let body = release("v1.0.0");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        assert_eq!(
            check_at(&format!("http://{address}/latest"), "0.1.0")
                .unwrap()
                .status,
            "update_available"
        );
        server.join().unwrap();
    }
}
