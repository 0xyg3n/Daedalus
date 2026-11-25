//! Local source control on the environment that owns the selected pane.
use super::{
    backend::Source,
    config::ControlConfig,
    project::{self, Change, Repository},
    session_menu::{Outcome, View},
};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
enum Stage {
    Target(Vec<Value>),
    Root,
    Files,
    File(String),
    Commit,
    Branch,
    Directory(String),
    Confirm(Box<project::Plan>, String),
    Output,
}
struct Pending {
    id: u64,
    mode: &'static str,
    choices: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
    pane: Option<Value>,
    repository: Option<Repository>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.pane = None;
        self.repository = None;
    }
    fn frame(
        &mut self,
        seq: &mut u64,
        mode: &'static str,
        title: &str,
        extra: Value,
        stage: Stage,
    ) -> Result<Outcome> {
        *seq = seq.checked_add(1).ok_or("Dialog sequence exhausted")?;
        let mut frame = json!({"version":2,"kind":"dialog","id":*seq,"mode":mode,"title":title});
        frame
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            choices,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    fn pane(&self) -> Result<Value> {
        self.pane
            .clone()
            .ok_or("Choose a project pane first".into())
    }
    fn root<S: Source>(
        &mut self,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let project::Reply::Repository(repository) =
            source.project(config, project::Request::Inspect(self.pane()?))?
        else {
            return Err("Invalid repository reply".into());
        };
        let description = project::display_text(&repository.summary());
        self.repository = Some(repository);
        self.frame(seq,"choose","Project · Git review",json!({"description":description,"options":[
            ["files","Changed files","Inspect a path, review its diff, stage or unstage it"],
            ["diff","Working tree diff","Review tracked edits without running external diff tools"],
            ["staged","Staged diff","Review exactly what the next commit includes"],
            ["commit","Commit staged changes","Enter a message, review the index, then confirm"],
            ["worktree","New branch and worktree","Create an isolated checkout at the reviewed HEAD"],
            ["branches","Local branches","Inspect current branches and tracking information"],
            ["worktrees","Worktrees","Inspect all linked checkout paths"],
            ["commits","Recent commits","Read the last 30 local commits"],
            ["refresh","Refresh","Read current repository state"],
            ["close","Close","Return to panes"]]}),Stage::Root)
    }
    pub fn start<S: Source>(
        &mut self,
        view: View,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        self.cancel();
        if let Some(pane) = view.selected {
            self.pane = Some(pane);
            return self.root(source, config, seq);
        }
        let panes = view.snapshot["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                p["cwd"].as_str().is_some_and(|s| !s.is_empty())
                    && !crate::adapters::pane_observations::conflicted(p)
            })
            .cloned()
            .collect::<Vec<_>>();
        if panes.is_empty() {
            return Err("Select a live pane with a project directory".into());
        }
        let options = panes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!([
                    i.to_string(),
                    project::display_text(&format!(
                        "{} · {}",
                        p["pane_id"].as_str().unwrap_or("?"),
                        p["title"].as_str().unwrap_or("Pane")
                    )),
                    project::display_text(p["cwd"].as_str().unwrap_or(""))
                ])
            })
            .collect::<Vec<_>>();
        self.frame(
            seq,
            "choose",
            "Choose the project environment",
            json!({"options":options}),
            Stage::Target(panes),
        )
    }
    fn output(&mut self, seq: &mut u64, text: String) -> Result<Outcome> {
        self.frame(
            seq,
            "output",
            "Project review",
            json!({"text":project::display_text(&text)}),
            Stage::Output,
        )
    }
    fn prepare<S: Source>(
        &mut self,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
        change: Change,
    ) -> Result<Outcome> {
        let project::Reply::Plan(plan) = source.project(
            config,
            project::Request::Prepare {
                pane: self.pane()?,
                change,
            },
        )?
        else {
            return Err("Invalid Git review reply".into());
        };
        let approval = plan.fingerprint()?;
        self.frame(seq,"choose","Apply this local Git change?",json!({"description":project::display_text(&plan.review()),"options":[["cancel","Cancel","Return to the project menu"],["apply","Apply reviewed change","The repository and target pane are checked again"]]}),Stage::Confirm(plan,approval))
    }
    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let p = self
            .pending
            .as_ref()
            .ok_or("No project dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(p.id) {
            return Err("Stale project reply".into());
        }
        let value = request.get("value").ok_or("Missing project reply")?;
        if value.as_str().is_some_and(|s| {
            s.len() > 4096 || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
        }) {
            return Err("Project input exceeds its text limit".into());
        }
        if !request["value"].is_null()
            && (request["value"].as_str().is_none()
                || (p.mode == "choose"
                    && !p
                        .choices
                        .iter()
                        .any(|v| Some(v.as_str()) == request["value"].as_str())))
        {
            return Err("Choose a listed project action".into());
        }
        let pending = self.pending.take().unwrap();
        if request["value"].is_null() {
            self.cancel();
            return Ok(Outcome::default());
        }
        let value = request["value"]
            .as_str()
            .ok_or("Project reply must be text")?;
        match pending.stage {
            Stage::Target(panes)=>{self.pane=Some(panes.get(value.parse::<usize>().map_err(|_|"Invalid pane")?).ok_or("Missing pane")?.clone());self.root(source,config,seq)},
            Stage::Root=>match value {
                "close"=>{self.cancel();Ok(Outcome::default())},
                "files"=>{
                    let repo=self.repository.as_ref().ok_or("Missing repository")?;
                    let options=repo.files.iter().enumerate().map(|(i,f)|json!([i.to_string(),project::display_text(&format!("{} {}",f.status,f.path)),f.original.as_deref().map(project::display_text).unwrap_or_default()])).collect::<Vec<_>>();
                    if options.is_empty(){return self.output(seq,"Working tree is clean".into());}
                    self.frame(seq,"choose","Changed files",json!({"options":options}),Stage::Files)
                },
                "diff"|"staged"=>{
                    let project::Reply::Text(text)=source.project(config,project::Request::Diff{pane:self.pane()?,path:None,staged:value=="staged"})? else{return Err("Invalid diff reply".into());};self.output(seq,text)
                },
                "commit"=>self.frame(seq,"input","Commit message",json!({"description":"Only staged changes are committed. The next step shows the reviewed index.","value":""}),Stage::Commit),
                "worktree"=>self.frame(seq,"input","New branch name",json!({"description":"Create a new branch and an isolated worktree from this checkout's HEAD.","value":""}),Stage::Branch),
                "branches"|"worktrees"|"commits"=>{
                    let project::Reply::Text(text)=source.project(config,project::Request::Report{pane:self.pane()?,kind:value.into()})? else{return Err("Invalid report reply".into());};self.output(seq,text)
                },
                "refresh"=>self.root(source,config,seq),
                _=>Err("Unknown Git action".into())
            },
            Stage::Files=>{
                let path=self.repository.as_ref().ok_or("Missing repository")?.files.get(value.parse::<usize>().map_err(|_|"Invalid file")?).ok_or("File disappeared")?.path.clone();
                self.frame(seq,"choose","File actions",json!({"description":project::display_text(&path),"options":[["diff","Working diff","Inspect unstaged tracked edits"],["staged","Staged diff","Inspect the index for this path"],["preview","View text file","Useful for untracked files"],["stage","Stage this path","Review the exact path and content fingerprint"],["unstage","Unstage this path","Preserve working files"],["back","Back to project",""]]}),Stage::File(path))
            },
            Stage::File(path)=>match value {
                "stage"=>self.prepare(source,config,seq,Change::Stage{path}),
                "unstage"=>self.prepare(source,config,seq,Change::Unstage{path}),
                "back"=>self.root(source,config,seq),
                _=>{
                    let action=if value=="preview"{project::Request::Preview{pane:self.pane()?,path}}else{project::Request::Diff{pane:self.pane()?,path:Some(path),staged:value=="staged"}};
                    let project::Reply::Text(text)=source.project(config,action)? else{return Err("Invalid file reply".into());};self.output(seq,text)
                }
            },
            Stage::Commit=>self.prepare(source,config,seq,Change::Commit{message:value.into()}),
            Stage::Branch=>self.frame(seq,"input","New worktree directory",json!({"description":"Enter a new absolute directory on this environment. Its parent must exist. Existing edits stay in the current checkout.","value":""}),Stage::Directory(value.into())),
            Stage::Directory(branch)=>self.prepare(source,config,seq,Change::Worktree{branch,directory:value.into()}),
            Stage::Confirm(plan,approval)=>{
                if value=="cancel"{return self.root(source,config,seq);}
                let project::Reply::Text(text)=source.project(config,project::Request::Apply{plan,approval})? else{return Err("Invalid Git result".into());};self.output(seq,text)
            },
            Stage::Output=>self.root(source,config,seq)
        }
    }
}
