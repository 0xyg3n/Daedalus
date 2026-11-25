//! Dialog contracts for the T3-inspired controls; all effects are synthetic.
use super::{
    backend::{Observations, Source},
    config::ControlConfig,
    orchestration_menu::{self, Run},
    project::{self, Repository},
    session_menu::View,
};
use crate::core::layouts::Layouts;
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
#[derive(Default)]
struct Fake {
    effects: usize,
    repository: Option<Repository>,
}
impl Source for Fake {
    fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
        Err("No live discovery in fixture".into())
    }
    fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
        Err("No terminal reads in fixture".into())
    }
    fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
        Err("No layouts in fixture".into())
    }
    fn project(&mut self, _: &ControlConfig, request: project::Request) -> Result<project::Reply> {
        match request {
            project::Request::Inspect(_) => {
                Ok(project::Reply::Repository(self.repository.clone().unwrap()))
            }
            project::Request::Prepare { pane, change } => {
                Ok(project::Reply::Plan(Box::new(project::Plan {
                    pane,
                    repository: self.repository.clone().unwrap(),
                    change,
                    file: None,
                    preview: "reviewed diff".into(),
                })))
            }
            project::Request::Apply { plan, approval } => {
                assert_eq!(approval, plan.fingerprint()?);
                self.effects += 1;
                Ok(project::Reply::Text("Fixture local change".into()))
            }
            _ => Ok(project::Reply::Text("Fixture read".into())),
        }
    }
    fn orchestrate(&mut self, _: &ControlConfig, run: Run) -> Result<orchestration_menu::Reply> {
        if let Run::Admit { plan, approval } = run {
            assert_eq!(approval, plan.fingerprint()?);
            assert_eq!(plan.tasks[0].session.as_deref(), Some("conversation"));
            assert_eq!(plan.tasks[0].identity.generation, "generation");
            self.effects += 1;
        }
        Ok(orchestration_menu::Reply {
            rows: Vec::new(),
            message: "Fixture plan saved".into(),
        })
    }
}
fn view() -> View {
    let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":"pane","terminal_id":"generation","agent":"codex","cwd":crate::test_support::path("/tmp/fixture"),"agent_status":"idle","agent_session":{"kind":"id","agent":"codex","value":"conversation"}});
    View {
        snapshot: json!({"panes":[pane]}),
        health: json!({}),
        layouts: Layouts::default(),
        selected: Some(pane),
        checkpoint: None,
        node: "fixture".into(),
    }
}
fn config() -> ControlConfig {
    ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
        "/tmp/fixture",
    )))
    .unwrap()
}
#[test]
fn orchestration_requires_complete_single_use_review_before_saving_any_task() {
    let mut menu = orchestration_menu::Menu::default();
    let mut source = Fake::default();
    let mut seq = 0;
    menu.start(view(), &mut seq).unwrap();
    let bad = json!({"op":"reply","id":seq,"value":"invalid"});
    assert!(menu.reply(&bad, &mut source, &config(), &mut seq).is_err());
    assert!(menu.pending());
    for value in [
        "add",
        "Fixture plan",
        "0",
        "Please review this isolated fixture",
        "done",
        "edit",
        "0",
        "prompt",
        "Please inspect this edited task prompt",
        "review",
    ] {
        menu.reply(
            &json!({"op":"reply","id":seq,"value":value}),
            &mut source,
            &config(),
            &mut seq,
        )
        .unwrap();
    }
    assert_eq!(source.effects, 0);
    let review_id = seq;
    let result = menu
        .reply(
            &json!({"op":"reply","id":review_id,"value":"run"}),
            &mut source,
            &config(),
            &mut seq,
        )
        .unwrap();
    assert_eq!(result.frames[0]["title"], "Plan saved");
    assert_eq!(source.effects, 1);
    assert!(
        menu.reply(
            &json!({"op":"reply","id":review_id,"value":"run"}),
            &mut source,
            &config(),
            &mut seq
        )
        .is_err()
    );
    assert_eq!(source.effects, 1);
}
#[test]
fn project_dialog_uses_standard_reply_protocol_and_never_replays_confirmation() {
    let mut menu = super::project_menu::Menu::default();
    let mut source = Fake {
        repository: Some(Repository {
            root: crate::test_support::path("/tmp/fixture").into(),
            git_dir: crate::test_support::path("/tmp/fixture/.git").into(),
            branch: "fixture".into(),
            head: "fixture-head".into(),
            files: Vec::new(),
            index: "fixture-index".into(),
            status: "fixture-status".into(),
        }),
        ..Default::default()
    };
    let mut seq = 0;
    menu.start(view(), &mut source, &config(), &mut seq)
        .unwrap();
    assert!(
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"not-listed"}),
            &mut source,
            &config(),
            &mut seq
        )
        .is_err()
    );
    assert!(menu.pending());
    for value in ["commit", "Fixture message"] {
        menu.reply(
            &json!({"op":"reply","id":seq,"value":value}),
            &mut source,
            &config(),
            &mut seq,
        )
        .unwrap();
    }
    assert_eq!(source.effects, 0);
    let review_id = seq;
    menu.reply(
        &json!({"op":"reply","id":review_id,"value":"apply"}),
        &mut source,
        &config(),
        &mut seq,
    )
    .unwrap();
    assert_eq!(source.effects, 1);
    assert!(
        menu.reply(
            &json!({"op":"reply","id":review_id,"value":"apply"}),
            &mut source,
            &config(),
            &mut seq
        )
        .is_err()
    );
    assert_eq!(source.effects, 1);
    menu.reply(
        &json!({"op":"reply","id":seq,"value":null}),
        &mut source,
        &config(),
        &mut seq,
    )
    .unwrap();
    assert!(!menu.pending());
}
