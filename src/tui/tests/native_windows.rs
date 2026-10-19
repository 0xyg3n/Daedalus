//! Windows-specific public API coverage. Cross-compilation checks this fixture;
//! it does not establish that ConPTY lifecycle works on a Windows host.
#![cfg(windows)]
use daedalus_tui::app::native_terminal::{self, StartOptions};
use std::collections::BTreeMap;

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
