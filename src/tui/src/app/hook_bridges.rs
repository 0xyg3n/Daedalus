//! Native harness runtime bridges. Generated code contains explicit selected
//! paths/identities only; model text is stdin JSON, never shell command source.
use super::hook_runtime::Registration;
use std::path::Path;
/// OpenCode v2 local server plugin. Native SDK methods supply canonical context;
/// raw terminal output and volatile idle notifications never authorize readiness.
pub fn opencode(registration: &Registration, binary: &Path) -> Result<Vec<u8>, String> {
    let config = serde_json::json!({"binary":binary,"registration":registration.path(),"session":registration.scope.conversation,"id":format!("daedalus.session-hooks.{}",registration.id)});
    let mut text = format!(
        "// Daedalus managed OpenCode bridge v2: {}\n",
        registration.id
    );
    text.push_str("import { Plugin } from '@opencode/plugin';\nimport { spawnSync } from 'node:child_process';\nimport { randomUUID } from 'node:crypto';\nconst config = ");
    text.push_str(
        &serde_json::to_string(&config).map_err(|_| "Cannot encode bridge configuration")?,
    );
    text.push_str(r#";
export default Plugin.define({id:config.id,async setup(ctx) {
  const boot=randomUUID();let sequence=0,closed=false;
  const controller=new AbortController();
  function emit(kind,fields={}) {
    const payload=JSON.stringify({bridge_version:'daedalus.opencode.v2',session_id:config.session,
      hook_event_name:kind,event_id:`${boot}-${++sequence}`,...fields});
    if(Buffer.byteLength(payload)>1048576)throw new Error('Daedalus hook payload exceeds limit');
    const result=spawnSync(config.binary,['hook','run','--registration',config.registration,'--event-kind',kind],
      {input:payload,encoding:'utf8',timeout:15000,maxBuffer:1048576,windowsHide:true,stdio:['pipe','pipe','pipe']});
    if(result.error||result.status!==0)throw new Error('Daedalus hook rejected event');
    return result.stdout.trim()?JSON.parse(result.stdout):{};
  }
  const session=await ctx.session.get({sessionID:config.session});
  if(session.id!==config.session)throw new Error('Daedalus session identity mismatch');
  const memory=emit('SessionStart').additional_context;
  const contextHook=await ctx.session.hook('context',event=>{
    if(event.sessionID!==config.session||typeof memory!=='string'||!memory)return;
    event.system.push({type:'text',text:memory});
  });
  const prompt=await ctx.session.hook('prompt',event=>{
    if(event.sessionID!==config.session)return;
    emit('UserPromptSubmit',{event_id:`prompt-${event.messageID}`,turn_id:event.messageID,prompt:event.prompt.text});
  });
  const tool=await ctx.tool.hook('execute.after',event=>{
    if(event.sessionID!==config.session)return;
    emit('PostToolUse',{event_id:`tool-${event.id}`,tool_name:event.tool});
  });
  void(async()=>{
    try {
      for await(const event of ctx.event.subscribe({signal:controller.signal})) {
        if(closed)break;
        if(event.data?.sessionID!==config.session)continue;
        if(event.type==='session.text.ended')emit('AssistantResponse',{event_id:event.id,text:event.data.text,
          native_event:event});
        if(event.type==='session.execution.succeeded') {
          let snapshot=null;
          try {
            const context=await ctx.session.context({sessionID:config.session});
            // Inspect a bounded suffix. Rust validates the exact prompt/idle
            // boundary and refuses missing, newer, or inconsistent evidence.
            if(Array.isArray(context))snapshot=context.slice(-512).map(m=>({id:m.id,type:m.type,text:m.text,outcome:m.outcome}));
          }catch{}
          emit('Stop',{event_id:event.id,status:'completed',native_event:event,context:snapshot});
        }
        if(event.type==='session.deleted'||event.type==='session.execution.failed'||event.type==='session.execution.interrupted')emit('SessionEnd',{event_id:event.id});
      }
    }catch{
      // A disconnected event stream cannot preserve a readiness claim.
    }finally{
      if(!closed){try{emit('SessionEnd');}catch{}}
    }
  })();
  return async()=>{closed=true;controller.abort();await prompt.dispose();await tool.dispose();await contextHook.dispose();try{emit('SessionEnd');}catch{}};
}});
"#);
    Ok(text.into_bytes())
}
pub fn pi(registration: &Registration, binary: &Path) -> Result<Vec<u8>, String> {
    let config = serde_json::json!({"binary":binary,"registration":registration.path(),"session":registration.scope.conversation,"transcript":registration.transcript});
    let mut text = format!("// Daedalus managed Pi bridge v1: {}\n", registration.id);
    text.push_str("import { spawnSync } from 'node:child_process';\nimport { randomUUID } from 'node:crypto';\nconst config = ");
    text.push_str(
        &serde_json::to_string(&config).map_err(|_| "Cannot encode bridge configuration")?,
    );
    text.push_str(r#";
export default function (pi) {
  const boot = randomUUID(); let sequence = 0, turn = null, memory = null;
  function emit(kind, ctx, fields = {}) {
    const session = ctx.sessionManager.getSessionId();
    if (session !== config.session) return {};
    const payload = JSON.stringify({bridge_version:'daedalus.pi.v1',session_id:session,
      hook_event_name:kind,event_id:`${boot}-${++sequence}`,turn_id:turn,
      transcript_path:config.transcript===null?null:(ctx.sessionManager.getSessionFile() ?? null),...fields});
    if (Buffer.byteLength(payload) > 1048576) throw new Error('Daedalus hook payload exceeds limit');
    const result = spawnSync(config.binary,['hook','run','--registration',config.registration,'--event-kind',kind],
      {input:payload,encoding:'utf8',timeout:15000,maxBuffer:1048576,windowsHide:true,stdio:['pipe','pipe','pipe']});
    if (result.error || result.status !== 0) throw new Error('Daedalus hook rejected event');
    return result.stdout.trim() ? JSON.parse(result.stdout) : {};
  }
  pi.on('session_start', (_event,ctx) => {turn=null;memory=emit('SessionStart',ctx).additional_context??null;});
  pi.on('before_agent_start', (event,ctx) => {
    if (ctx.sessionManager.getSessionId()!==config.session) return;
    turn=randomUUID();emit('UserPromptSubmit',ctx,{prompt:event.messageType==='user'?event.prompt:null});
    if (typeof memory==='string') { const content=memory;memory=null;return {message:{customType:'daedalus-memory',content,display:false}}; }
  });
  pi.on('turn_start', (_event,ctx) => {emit('Activity',ctx);});
  pi.on('tool_execution_end', (event,ctx) => {emit('PostToolUse',ctx,{tool_name:event.toolName});});
  pi.on('message_end', (event,ctx) => {
    if (event.message.role !== 'assistant') return;
    const text=event.message.content.filter(part=>part.type==='text').map(part=>part.text).join('\n');
    if (text) emit('AssistantResponse',ctx,{text});
  });
  pi.on('agent_before_settle', (event,ctx) => {
    if (event.outcome!=='completed') return;
    const result=emit('Stop',ctx,{status:event.outcome});
    if (Array.isArray(result.entries)) result.entries=[...(event.entries??[]),...result.entries];
    return result;
  });
  pi.on('agent_settled', (event,ctx) => {emit('Settled',ctx,{status:event.aborted?'aborted':'settled'});});
  pi.on('session_shutdown', (_event,ctx) => {emit('SessionEnd',ctx);turn=null;});
}
"#);
    Ok(text.into_bytes())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::app::hooks::Harness;

    #[test]
    fn native_bridges_hide_console_and_preserve_hook_failures() {
        let mut nonce = [0; 16];
        crate::platform::random_bytes(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "daedalus-hidden-hooks-{}-{:x}",
            std::process::id(),
            u128::from_le_bytes(nonce)
        ));
        crate::platform::ensure_private_dir(&root).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut registration: Registration = serde_json::from_value(serde_json::json!({
            "version":1,"id":"fixture","directory":root,
            "scope":{"instance":"fixture","harness":"opencode","conversation":"session",
                "terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"}},
            "goal_id":null,"harness_process":null,"transcript":null,"events":["SessionStart","UserPromptSubmit","Stop"],
            "options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},
            "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000
        })).unwrap();
        for harness in [Harness::Opencode, Harness::Pi] {
            registration.scope.harness = harness;
            let source = if harness == Harness::Opencode {
                opencode(&registration, &executable)
            } else {
                pi(&registration, &executable)
            }
            .unwrap();
            let bridge = root.join("bridge.mjs");
            // Keep the real generated spawn options. Replace only the external
            // provider and receiver with an actual native console-probing child.
            let receiver = r#"import {spawnSync as nativeSpawnSync} from 'node:child_process';
let calls=0;export function count(){return calls;}
const spawnSync=(binary,args,options)=>{
  calls++;
  if(globalThis.rejectHook)return {status:17,stdout:'',stderr:'fixture failure'};
  const result=nativeSpawnSync(binary,['--ignored','--exact','platform::windows::tests::hidden_console_child_fixture','--nocapture'],options);
  return {...result,stdout:result.status===0?'{}':result.stdout};
};"#;
            let source = String::from_utf8(source)
                .unwrap()
                .replace(
                    "import { Plugin } from '@opencode/plugin';",
                    "const Plugin={define:value=>value};",
                )
                .replace("import { spawnSync } from 'node:child_process';", receiver);
            fs::write(&bridge, source).unwrap();
            let runner = root.join("run.mjs");
            let activate = if harness == Harness::Opencode {
                "await bridge.setup(ctx);await handlers.prompt({sessionID:'session',messageID:'msg_fixture',prompt:{text:'fixture'}});"
            } else {
                "bridge({on:(name,handler)=>{handlers[name]=handler;}});handlers.session_start({},ctx);handlers.before_agent_start({messageType:'user',prompt:'fixture'},ctx);"
            };
            let failure = if harness == Harness::Opencode {
                "await handlers.prompt({sessionID:'session',messageID:'msg_failure',prompt:{text:'fixture'}});"
            } else {
                "handlers.before_agent_start({messageType:'user',prompt:'fixture'},ctx);"
            };
            fs::write(&runner, format!(r#"import bridge,{{count}} from './bridge.mjs';
const handlers={{}};
const ctx={{session:{{get:async()=>({{id:'session'}}),hook:async(name,handler)=>{{handlers[name]=handler;return{{dispose:async()=>{{}}}};}}}},
tool:{{hook:async()=>({{dispose:async()=>{{}}}})}},event:{{async *subscribe(){{}}}},
sessionManager:{{getSessionId:()=> 'session'}}}};
{activate}
if(count()<2)throw new Error('Native child fixtures did not execute');
globalThis.rejectHook=true;let rejected=false;
try{{{failure}}}catch(error){{rejected=error.message==='Daedalus hook rejected event';}}
if(!rejected)throw new Error('Hook failure was hidden');
"#)).unwrap();
            let mut command = std::process::Command::new("node");
            command.arg(&runner);
            let output = crate::adapters::process::run(
                &mut command,
                std::time::Duration::from_secs(30),
                16384,
            )
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    use std::fs;
}
