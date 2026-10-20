use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = manifest.join("../../assets/daedalus.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    let icon = fs::canonicalize(&icon).unwrap_or_else(|_| panic!("missing {}", icon.display()));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let rc_path = out_dir.join("daedalus.rc");
    let icon_rc = icon.to_string_lossy().replace('\\', "/");
    fs::write(&rc_path, format!("1 ICON \"{icon_rc}\"\n")).expect("write icon script");
    let env_name = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let object = out_dir.join(if env_name == "msvc" {
        "daedalus.res"
    } else {
        "daedalus-icon.o"
    });
    let tool = if env_name == "msvc" {
        "rc"
    } else {
        match arch.as_str() {
            "x86_64" => "x86_64-w64-mingw32-windres",
            "aarch64" => "aarch64-w64-mingw32-windres",
            _ => "windres",
        }
    };
    let mut cmd = Command::new(tool);
    if env_name == "msvc" {
        cmd.arg("/nologo").arg("/fo").arg(&object).arg(&rc_path);
    } else {
        cmd.args(["-O", "coff", "-i"])
            .arg(&rc_path)
            .arg("-o")
            .arg(&object);
    }
    let status = cmd
        .status()
        .unwrap_or_else(|error| panic!("cannot run {tool} to embed the Daedalus icon: {error}"));
    if !status.success() {
        panic!("{tool} failed to embed the Daedalus icon");
    }
    println!("cargo:rustc-link-arg-bin=daedalus-tui={}", object.display());
}
