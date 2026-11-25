//! TUI plans keep the user's exact pane selection and review before admission.
use super::{
    backend::Source,
    config::ControlConfig,
    orchestration::{Plan, Row, Task},
    session_menu::{Outcome, View},
};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
pub enum Run {
    List,
    Admit { plan: Plan, approval: String },
    Cancel { plan: String, key: String },
    Resolve { plan: String, key: String },
    Wake,
}
pub struct Reply {
    pub rows: Vec<Row>,
    pub message: String,
}
enum Stage {
    Root,
    Name,
    Target(Vec<Value>),
    Prompt(Value),
    Dependencies(Task),
    Review(String),
    DraftTasks,
    DraftTask(usize),
    EditPrompt(usize),
    Remove(usize),
    Rows(Vec<Row>),
    Task(Row),
    Cancel(Row),
    Resolve(Row),
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
    view: Option<View>,
    draft: Option<Plan>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.view = None;
        self.draft = None;
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
        for field in ["title", "description", "text"] {
            if let Some(value) = frame[field].as_str() {
                frame[field] = json!(super::project::display_text(value));
            }
        }
        if let Some(options) = frame["options"].as_array_mut() {
            for option in options {
                for i in [1, 2] {
                    if let Some(value) = option[i].as_str() {
                        option[i] = json!(super::project::display_text(value));
                    }
                }
            }
        }
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
    fn root(&mut self, seq: &mut u64) -> Result<Outcome> {
        let text=self.draft.as_ref().map(|p|format!("{} · {} tasks · up to {} agents in parallel",p.name,p.tasks.len(),p.parallel)).unwrap_or_else(||"Build a dependency plan across exact native agent panes. Plans keep running when the TUI closes.".into());
        self.frame(seq,"choose","Agent orchestration",json!({"description":text,"options":[["add","Add a task","Choose an exact agent pane, write its prompt and dependencies"],["edit","Edit draft tasks","Change prompts, dependencies or remove an unsaved task"],["review","Review and run plan","Review every destination and prompt before any send"],["parallel","Parallel limit","Cycle between 1, 2, 4 and 8 concurrent tasks"],["runs","Plans and task history","Inspect queued, running, approval-waiting and uncertain tasks"],["worker","Resume queued work","Start the instance worker; uncertain sends are never retried"],["close","Close","Saved plans and active agents keep running"]]}),Stage::Root)
    }
    pub fn start(&mut self, view: View, seq: &mut u64) -> Result<Outcome> {
        self.view = Some(view);
        self.root(seq)
    }
    fn targets(&mut self, seq: &mut u64) -> Result<Outcome> {
        let panes = self.view.as_ref().ok_or("Missing plan scope")?.snapshot["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                p["agent"]
                    .as_str()
                    .and_then(crate::adapters::harnesses::canonical_id)
                    .is_some()
                    && crate::adapters::terminals::reported_session(p).is_some()
                    && !crate::adapters::pane_observations::conflicted(p)
            })
            .cloned()
            .collect::<Vec<_>>();
        if panes.is_empty() {
            return self.frame(seq,"output","No verified agent conversations",json!({"text":"Start an agent with native hooks, or select a Herdr agent with a reported conversation and lifecycle state. Shells and historical sessions cannot receive scheduled tasks."}),Stage::Output);
        }
        let options = panes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!([
                    i.to_string(),
                    format!(
                        "{} · {}",
                        p["pane_id"].as_str().unwrap_or("?"),
                        p["title"].as_str().unwrap_or("Agent")
                    ),
                    format!(
                        "{} · {}",
                        p["agent"].as_str().unwrap_or("?"),
                        p["cwd"].as_str().unwrap_or("?")
                    )
                ])
            })
            .collect::<Vec<_>>();
        self.frame(seq,"choose","Send this task to which agent?",json!({"options":options,"description":"The terminal generation, provider and current conversation are captured. Busy agents wait until idle; changed identities are not substituted."}),Stage::Target(panes))
    }
    fn dependencies(&mut self, seq: &mut u64, task: Task) -> Result<Outcome> {
        let mut options = vec![json!([
            "done",
            "Dependencies selected",
            "Continue building the plan"
        ])];
        for t in &self.draft.as_ref().ok_or("Missing draft")?.tasks {
            if t.key == task.key {
                continue;
            }
            options.push(json!([
                t.key,
                format!(
                    "{} {}",
                    if task.dependencies.contains(&t.key) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    t.label
                ),
                "Toggle: wait for this task's observed turn to end"
            ]));
        }
        self.frame(seq,"choose","Task dependencies",json!({"options":options,"description":"No dependencies allows this task to run in parallel on its own pane. A failed, cancelled or uncertain dependency skips dependent tasks."}),Stage::Dependencies(task))
    }
    pub fn reply(
        &mut self,
        request: &Value,
        source: &mut impl Source,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let p = self.pending.as_ref().ok_or("No orchestration dialog")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(p.id) {
            return Err("Unexpected orchestration reply".into());
        }
        let value = request.get("value").ok_or("Missing reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid reply")?;
            if (p.mode == "choose" && !p.choices.iter().any(|c| c == text))
                || text.len() > 16000
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err("Invalid orchestration reply".into());
            }
        }
        let p = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome::default());
        }
        let text = value.as_str().unwrap();
        match p.stage {
            Stage::Root=>match text {
                "close"=>{self.cancel();Ok(Outcome::default())},
                "add"=>{
                    if self.draft.as_ref().is_some_and(|p|p.tasks.len()>=32){return Err("A plan can contain at most 32 tasks".into());}
                    if self.draft.is_none(){self.frame(seq,"input","Plan name",json!({"description":"A private, durable plan for this environment."}),Stage::Name)}else{self.targets(seq)}
                },
                "parallel"=>{
                    if let Some(p)=self.draft.as_mut(){p.parallel=match p.parallel{1=>2,2=>4,4=>8,_=>1};}
                    self.root(seq)
                },
                "edit"=>{
                    let plan=self.draft.as_ref().filter(|p|!p.tasks.is_empty()).ok_or("Add a task first")?;
                    let options=plan.tasks.iter().enumerate().map(|(i,t)|json!([i.to_string(),t.label,format!("{} · {} · after {}",t.identity.pane,t.agent,t.dependencies.join(", "))])).collect::<Vec<_>>();
                    self.frame(seq,"choose","Draft tasks",json!({"options":options}),Stage::DraftTasks)
                },
                "review"=>{
                    let plan=self.draft.as_ref().ok_or("Add a task first")?;
                    let approval=plan.fingerprint()?;
                    let mut review=format!("{}\nParallel limit: {}\n\n",plan.name,plan.parallel);
                    for task in &plan.tasks {review.push_str(&format!("{} · {} · {:?} / {} / {}\nConversation: {}\nAfter: {}\nPrompt:\n{}\n\n",task.key,task.agent,task.identity.backend,task.identity.endpoint,task.identity.pane,task.session.as_deref().unwrap_or("not yet reported"),task.dependencies.join(", "),task.prompt));}
                    review.push_str("Each prompt is submitted once to its exact pane. Submission is not completion. Approval-waiting agents stay pending; uncertain sends require manual review and are never retried.");
                    self.frame(seq,"choose","Run this orchestration plan?",json!({"description":review,"options":[["cancel","Back","No messages sent"],["run","Save plan and run","Persist intent and start the native worker"]]}),Stage::Review(approval))
                },
                "runs"=>{
                    let reply=source.orchestrate(config,Run::List)?;
                    let options=reply.rows.iter().take(200).enumerate().map(|(i,r)|json!([i.to_string(),format!("{} · {}",r.state,r.task.label),format!("{} · {}",r.plan,r.task.identity.pane)])).collect::<Vec<_>>();
                    if options.is_empty(){return self.frame(seq,"output","No saved plans",json!({"text":"Add tasks and review a plan before running it."}),Stage::Output);}
                    self.frame(seq,"choose","Orchestration tasks",json!({"options":options,"description":"Completed means a native turn was observed working and then ended; it does not certify the task's semantic success."}),Stage::Rows(reply.rows))
                },
                "worker"=>{
                    let result=source.orchestrate(config,Run::Wake)?;
                    self.frame(seq,"output","Orchestration worker",json!({"text":result.message}),Stage::Output)
                },
                _=>Err("Unknown plan action".into()),
            },
            Stage::Name=>{
                if text.trim().is_empty() || text.len()>160 || text.chars().any(char::is_control){return Err("Use a printable plan name, up to 160 bytes".into());}
                self.draft=Some(Plan{id:super::orchestration::new_id(),name:text.into(),parallel:2,tasks:Vec::new()});
                self.targets(seq)
            },
            Stage::Target(panes)=>{
                let pane=panes.get(text.parse::<usize>().map_err(|_|"Invalid pane")?).ok_or("Pane disappeared")?.clone();
                self.frame(seq,"input","Task prompt",json!({"description":format!("Destination: {} · {}\nEnter the exact request to submit (16–16000 bytes).",pane["agent"].as_str().unwrap_or("Agent"),pane["pane_id"].as_str().unwrap_or("?"))}),Stage::Prompt(pane))
            },
            Stage::Prompt(pane)=>{
                if text.trim().len()<16{return Err("Enter a task prompt of at least 16 bytes".into());}
                let plan=self.draft.as_ref().ok_or("Missing draft")?;
                let number=(1..=33).find(|i|!plan.tasks.iter().any(|t|t.key==format!("task-{i}"))).ok_or("Too many tasks")?;
                let task=Task{key:format!("task-{number}"),label:text.lines().next().unwrap_or(text).chars().take(40).collect(),identity:crate::adapters::terminals::identity(&pane)?,
                    agent:pane["agent"].as_str().ok_or("Missing agent")?.into(),session:crate::adapters::terminals::reported_session(&pane).map(str::to_owned),pane,prompt:text.into(),dependencies:Vec::new()};
                self.dependencies(seq,task)
            },
            Stage::Dependencies(mut task)=>{
                if text=="done"{let tasks=&mut self.draft.as_mut().ok_or("Missing draft")?.tasks;if let Some(existing)=tasks.iter_mut().find(|t|t.key==task.key){*existing=task;}else{tasks.push(task);}self.root(seq)}
                else {if task.dependencies.iter().any(|d|d==text){task.dependencies.retain(|d|d!=text);}else{task.dependencies.push(text.into());}self.dependencies(seq,task)}
            },
            Stage::Review(approval)=>{
                if text=="cancel"{return self.root(seq);}
                let result=source.orchestrate(config,Run::Admit{plan:self.draft.as_ref().ok_or("Missing draft")?.clone(),approval})?;
                self.draft=None;
                self.frame(seq,"output","Plan saved",json!({"text":result.message}),Stage::Output)
            },
            Stage::DraftTasks=>{
                let i=text.parse::<usize>().map_err(|_|"Invalid draft task")?;
                let task=self.draft.as_ref().and_then(|p|p.tasks.get(i)).ok_or("Draft task disappeared")?;
                self.frame(seq,"choose","Edit draft task",json!({"description":format!("{} · {}\n{}",task.key,task.identity.pane,task.prompt),"options":[["prompt","Edit prompt","Only the unsaved draft changes"],["dependencies","Edit dependencies","A dependency cycle is refused at review"],["remove","Remove draft task","Review dependency changes before removing"],["back","Back","Return to plan"]]}),Stage::DraftTask(i))
            },
            Stage::DraftTask(i)=>{
                let task=self.draft.as_ref().and_then(|p|p.tasks.get(i)).ok_or("Draft task disappeared")?.clone();
                match text {
                    "prompt"=>self.frame(seq,"input","Edit task prompt",json!({"value":task.prompt,"description":"16–16000 bytes. The saved queue is unchanged until you review and run the whole plan."}),Stage::EditPrompt(i)),
                    "dependencies"=>self.dependencies(seq,task),
                    "remove"=>self.frame(seq,"choose","Remove this draft task?",json!({"description":format!("{}\nDependency links to this task will also be removed. Review the complete plan before running.",task.label),"options":[["no","Keep task","No change"],["yes","Remove task and its dependency links","This affects only the unsaved draft"]]}),Stage::Remove(i)),
                    _=>self.root(seq)
                }
            },
            Stage::EditPrompt(i)=>{
                if text.trim().len()<16{return Err("Enter at least 16 bytes".into());}
                let task=self.draft.as_mut().and_then(|p|p.tasks.get_mut(i)).ok_or("Draft task disappeared")?;
                task.prompt=text.into();task.label=text.lines().next().unwrap_or(text).chars().take(40).collect();self.root(seq)
            },
            Stage::Remove(i)=>{
                if text=="yes"{let plan=self.draft.as_mut().ok_or("Missing draft")?;if i>=plan.tasks.len(){return Err("Draft task disappeared".into());}let removed=plan.tasks.remove(i);for task in &mut plan.tasks {task.dependencies.retain(|d|d!=&removed.key);}}
                self.root(seq)
            },
            Stage::Rows(rows)=>{
                let row=rows.get(text.parse::<usize>().map_err(|_|"Invalid task")?).ok_or("Task disappeared")?.clone();
                let mut options=vec![json!(["details","Details and prompt","Inspect the captured destination and dependencies"]),json!(["back","Back","Refresh task state"])];
                if row.state=="queued"{options.insert(1,json!(["cancel","Cancel queued task","Does not interrupt a running agent"]));}
                if row.state=="unconfirmed"{options.insert(1,json!(["resolve","Mark inspected; release pane","Inspect the exact idle agent first. This task is cancelled, never replayed."]));}
                self.frame(seq,"choose",&format!("{} · {}",row.state,row.task.label),json!({"options":options}),Stage::Task(row))
            },
            Stage::Task(row)=>match text {
                "back"=>self.root(seq),
                "cancel"=>self.frame(seq,"choose","Cancel this queued task?",json!({"description":format!("{} · {}",row.task.label,row.task.identity.pane),"options":[["no","Keep task","No change"],["yes","Cancel queued task","Dependent tasks will be skipped"]]}),Stage::Cancel(row)),
                "resolve"=>self.frame(seq,"choose","Have you inspected the original agent?",json!({"description":format!("{} · {} · {}\nThe original conversation must be idle. This task will be cancelled and dependent tasks skipped. Other queued work can then use this pane. Nothing is replayed.",row.task.identity.endpoint,row.task.identity.pane,row.task.session.as_deref().unwrap_or("?")),"options":[["no","Keep uncertainty","Pane remains reserved"],["yes","Inspected; release pane","Cancel this task without sending its prompt again"]]}),Stage::Resolve(row)),
                "details"=>self.frame(seq,"output","Task details",json!({"text":serde_json::to_string_pretty(&row).map_err(|_|"Cannot display task")?}),Stage::Output),
                _=>Err("Invalid task action".into()),
            },
            Stage::Cancel(row)=>{if text=="yes"{source.orchestrate(config,Run::Cancel{plan:row.plan,key:row.task.key})?;}self.root(seq)},
            Stage::Resolve(row)=>{if text=="yes"{source.orchestrate(config,Run::Resolve{plan:row.plan,key:row.task.key})?;}self.root(seq)},
            Stage::Output=>self.root(seq),
        }
    }
}
