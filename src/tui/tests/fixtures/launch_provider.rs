//! Inert native provider for first-launch tests. No network or provider calls.
use std::{fs, path::PathBuf, time::Duration};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("Claude fixture 2.1.295");
        return;
    }
    let value = |flag: &str| {
        let index = args.iter().position(|s| s == flag).expect("expected launch flag");
        args.get(index + 1).expect("expected launch value")
    };
    let session = value("--session-id");
    let plugin = PathBuf::from(value("--plugin-dir"));
    let id = format!("launch-{session}");
    let directory = plugin.parent().unwrap().parent().unwrap();
    let registration = directory.join("hooks").join(format!("{id}.json"));
    assert!(registration.is_file(), "registration must precede provider exec");
    assert!(plugin.join("hooks/hooks.json").is_file());
    fs::write(directory.join("provider-started"), std::process::id().to_string()).unwrap();
    println!("FIXTURE_PROVIDER_REGISTERED");
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
