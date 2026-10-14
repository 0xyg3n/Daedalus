//! Repository and installation acceptance without a Python runtime or network.
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn tracked_candidates() -> Vec<PathBuf> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root())
        .output()
        .unwrap();
    assert!(output.status.success());
    output
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| root().join(std::str::from_utf8(s).unwrap()))
        .filter(|p| p.exists())
        .collect()
}
#[test]
fn package_has_no_legacy_runtime_or_private_state() {
    let root = root();
    for path in tracked_candidates() {
        let relative = path.strip_prefix(&root).unwrap();
        let name = relative.to_string_lossy();
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        assert_ne!(extension, "py", "Python source remains: {name}");
        assert!(
            ![
                "db", "sqlite", "sqlite3", "jsonl", "log", "pem", "key", "token"
            ]
            .contains(&extension),
            "Runtime artifact: {name}"
        );
        assert!(
            !relative.components().any(|c| [
                "node_modules",
                ".venv",
                "__pycache__",
                ".omc",
                "audits"
            ]
            .contains(&c.as_os_str().to_str().unwrap_or(""))),
            "Generated state: {name}"
        );
        if name.starts_with("tests/") || name.starts_with("src/tui/tests/") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            let production = if extension == "rs" {
                text.split("#[cfg(test)]").next().unwrap()
            } else {
                &text
            };
            // Only placeholder home directories may appear in shipped text.
            for (_, rest) in production
                .match_indices("/home/")
                .map(|(at, _)| production.split_at(at + 6))
            {
                let user: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
                    .collect();
                assert!(
                    ["example", "hostname", "user"].contains(&user.as_str()),
                    "Private home directory in {name}"
                );
            }
        }
    }
    for obsolete in [
        "src/control",
        "src/comms",
        "src/fleet",
        "src/hooks",
        "src/security",
        "src/self_improve",
        "src/heartbeat",
        "src/browser",
        "src/monitoring",
        "src/integrations",
    ] {
        assert!(
            !tracked_candidates()
                .iter()
                .any(|p| p.strip_prefix(&root).unwrap().starts_with(obsolete)),
            "Legacy runtime directory: {obsolete}"
        );
    }
}
#[test]
fn shell_entrypoints_parse_and_do_not_start_python() {
    for path in tracked_candidates() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.lines().next().is_some_and(|line| {
            line.starts_with("#!") && (line.contains("bash") || line.ends_with("/sh"))
        }) {
            let output = Command::new("/bin/bash")
                .arg("-n")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !text.contains("python3") && !text.contains("DAEDALUS_PYTHON"),
                "Python entrypoint: {}",
                path.display()
            );
        }
    }
}
fn local_target(path: &Path, target: &str) {
    let target = target.trim().trim_matches(['<', '>']);
    if target.is_empty()
        || target.starts_with('#')
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return;
    }
    let target = target.split('#').next().unwrap().split('?').next().unwrap();
    if target.is_empty() {
        return;
    }
    let target = target.replace("%20", " ");
    assert!(
        path.parent().unwrap().join(&target).exists(),
        "{}: broken local link {target}",
        path.display()
    );
}
#[test]
fn documentation_links_resolve_after_legacy_removal() {
    for path in tracked_candidates()
        .into_iter()
        .filter(|p| p.extension().is_some_and(|s| s == "md"))
    {
        let text = fs::read_to_string(&path).unwrap();
        let mut fenced = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            let mut remaining = line;
            while let Some((_, tail)) = remaining.split_once("](") {
                let Some((target, rest)) = tail.split_once(')') else {
                    break;
                };
                local_target(&path, target);
                remaining = rest;
            }
            for attribute in ["src=\"", "href=\"", "src='", "href='"] {
                let mut remaining = line;
                let quote = attribute.chars().last().unwrap();
                while let Some((_, tail)) = remaining.split_once(attribute) {
                    let Some((target, rest)) = tail.split_once(quote) else {
                        break;
                    };
                    local_target(&path, target);
                    remaining = rest;
                }
            }
        }
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "daedalus-package-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn install(&self, arguments: &[&str]) -> Output {
        Command::new("/bin/bash")
            .arg(root().join("install.sh"))
            .args([
                "--prefix",
                self.0.join("prefix with spaces").to_str().unwrap(),
            ])
            .args(arguments)
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }
    fn binary(&self) -> (PathBuf, String) {
        let binary = self.0.join("binary");
        fs::copy(env!("CARGO_BIN_EXE_daedalus-tui"), &binary).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&binary).unwrap()));
        (binary, digest)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn installer_preview_rejects_retired_runtime_and_writes_nothing() {
    let fixture = Fixture::new();
    let output = fixture.install(&["--plan", "--no-deps"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("No Python runtime"));
    assert!(!fixture.0.join("prefix with spaces").exists());
    assert!(
        !fixture
            .install(&["--compatibility", "--plan"])
            .status
            .success()
    );
    assert!(!fixture.0.join("prefix with spaces").exists());
}
#[test]
fn installer_checks_hash_architecture_symlinks_and_existing_ownership() {
    let fixture = Fixture::new();
    let (binary, digest) = fixture.binary();
    let binary = binary.to_str().unwrap();
    assert!(
        !fixture
            .install(&["--binary", binary, "--sha256", &"0".repeat(64), "--yes"])
            .status
            .success()
    );
    let alias = fixture.0.join("alias");
    symlink(binary, &alias).unwrap();
    assert!(
        !fixture
            .install(&[
                "--binary",
                alias.to_str().unwrap(),
                "--sha256",
                &digest,
                "--yes"
            ])
            .status
            .success()
    );
    let args = ["--binary", binary, "--sha256", &digest, "--yes"];
    for _ in 0..2 {
        let output = fixture.install(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let command = fixture.0.join("prefix with spaces/bin/daedalus");
    assert!(
        fs::symlink_metadata(&command)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&command).unwrap();
    fs::write(&command, b"operator-owned").unwrap();
    assert!(!fixture.install(&args).status.success());
    assert_eq!(fs::read(&command).unwrap(), b"operator-owned");
    let mut bytes = fs::read(binary).unwrap();
    bytes[18..20].copy_from_slice(&[0, 0]);
    fs::write(binary, &bytes).unwrap();
    let changed = format!("{:x}", Sha256::digest(&bytes));
    let output = fixture.install(&["--binary", binary, "--sha256", &changed, "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ELF"));
}
#[test]
fn installer_detects_architecture_before_installing_or_running_build_tools() {
    let fixture = Fixture::new();
    let tools = fixture.0.join("tools");
    fs::create_dir(&tools).unwrap();
    let uname = tools.join("uname");
    let execute = || {
        Command::new("/bin/bash")
            .arg(root().join("install.sh"))
            .args([
                "--prefix",
                fixture.0.join("untouched").to_str().unwrap(),
                "--plan",
            ])
            .env_clear()
            .env("HOME", &fixture.0)
            .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
            .output()
            .unwrap()
    };
    fs::write(
        &uname,
        b"#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo aarch64;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&uname, fs::Permissions::from_mode(0o755)).unwrap();
    let output = execute();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Linux aarch64"));
    fs::write(&uname, b"#!/bin/sh\necho unsupported\n").unwrap();
    assert!(!execute().status.success());
    assert!(!fixture.0.join("untouched").exists());
}
#[test]
fn source_wrappers_always_use_native_binary_and_fail_closed_without_it() {
    let fixture = Fixture::new();
    let checkout = fixture.0.join("checkout");
    let bin = checkout.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let native = checkout.join("src/tui/target/release/daedalus-tui");
    fs::create_dir_all(native.parent().unwrap()).unwrap();
    fs::write(&native, b"#!/bin/sh\nprintf 'native:%s\\n' \"$1\"\n").unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o755)).unwrap();
    for (name, arguments, expected) in [
        ("daedalus", vec!["--help"], "native:--help"),
        ("jobctl", vec!["--help"], "native:job"),
        ("tma", vec!["--help"], "native:tma"),
    ] {
        let wrapper = bin.join(name);
        fs::copy(root().join("bin").join(name), &wrapper).unwrap();
        let run = || {
            Command::new("/bin/bash")
                .arg(&wrapper)
                .args(&arguments)
                .env_clear()
                .env("HOME", &fixture.0)
                .env("PATH", "/usr/bin:/bin")
                .env("DAEDALUS_PYTHON", "/nonexistent/python")
                .output()
                .unwrap()
        };
        let output = run();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        let hidden = native.with_extension("hidden");
        fs::rename(&native, &hidden).unwrap();
        let output = run();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Build Daedalus first"));
        fs::rename(&hidden, &native).unwrap();
    }
}

#[test]
fn committed_source_install_uses_locked_release_and_never_calls_python() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("source");
    fs::create_dir(&repository).unwrap();
    fs::write(repository.join("README"), b"isolated committed source").unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["add", "README"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .env("HOME", &fixture.0)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    let tools = fixture.0.join("tools");
    fs::create_dir(&tools).unwrap();
    for (name, bytes) in [
        (
            "cargo",
            br#"#!/bin/bash
set -eu
[[ "$*" == *"--release --locked"* ]]
while (($#)); do if [[ "$1" == --target-dir ]]; then target="$2"; break; fi; shift; done
mkdir -p "$target/release"
cp "$FIXTURE_ELF" "$target/release/daedalus-tui"
"#
            .as_slice(),
        ),
        ("rustc", b"#!/bin/sh\necho 'rustc 1.88.0'\n".as_slice()),
        (
            "python3",
            b"#!/bin/sh\necho 'unexpected Python' >&2\nexit 99\n".as_slice(),
        ),
    ] {
        let path = tools.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let prefix = fixture.0.join("installed");
    let output = Command::new("/bin/bash")
        .arg(root().join("install.sh"))
        .arg("--source")
        .arg(&repository)
        .arg("--prefix")
        .arg(&prefix)
        .args(["--no-deps", "--yes"])
        .env_clear()
        .env("HOME", &fixture.0)
        .env("CARGO_HOME", fixture.0.join("cargo"))
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
        .env("FIXTURE_ELF", env!("CARGO_BIN_EXE_daedalus-tui"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(prefix.join("bin/daedalus"))
        .arg("--version")
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("daedalus "));
    assert_eq!(fs::read_dir(prefix.join("bin")).unwrap().count(), 1);
}
