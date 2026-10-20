mod backdrop;
#[cfg(any(windows, test))]
mod clipboard;
mod embed;
mod graphics;
mod model;
mod neon;
mod options;
mod shortcuts;
#[cfg(test)]
mod test_support;
mod theme;
mod update_ui;
mod view;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
};
use embed::{Embed, Mode, Plan};
use model::{
    COMMANDS, Dialog, Kind, OVERVIEW, OptionsMenu, Order, Preview, Sizes, Snapshot, Splash, Split,
    Ui, View, WORKSPACES,
};
use options::Item;
use serde_json::{Value, json};
use std::{
    io::{self, BufReader, IsTerminal, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const PROTOCOL: u64 = 2;
/// How long quitting waits for the backend to save the view.
const QUIT_WAIT: Duration = Duration::from_millis(1500);
/// Previews refresh at most this often, for on-screen terminals only.
const PREVIEW_EVERY: Duration = Duration::from_millis(2000);
/// A newly shown terminal is read this soon after the previous read.
const PREVIEW_SETTLE: Duration = Duration::from_millis(250);
/// Resized panels are remembered once the sizes stop changing.
const SIZES_SETTLE: Duration = Duration::from_millis(600);

struct Host {
    child: Child,
    input: ChildStdin,
}
impl Host {
    fn send(&mut self, mut value: Value) -> io::Result<()> {
        value["version"] = json!(PROTOCOL);
        writeln!(self.input, "{value}")?;
        self.input.flush()
    }
    /// Give the backend up to `limit` to finish its last request (saving
    /// the view on quit) and exit; whatever still runs is stopped on drop.
    fn wait_exit(&mut self, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => thread::sleep(Duration::from_millis(15)),
            }
        }
        false
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn parse_args(args: Vec<String>) -> io::Result<(bool, Vec<String>)> {
    let mut setup = false;
    let mut backend = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "tui" => {}
            "setup" => setup = true,
            "--demo" => backend.push(arg),
            "--config-dir" => {
                backend.push(arg);
                backend.push(
                    args.next()
                        .ok_or_else(|| io::Error::other("--config-dir requires a path"))?,
                );
            }
            _ => {
                return Err(io::Error::other(
                    "Unknown UI option; use daedalus tui --help",
                ));
            }
        }
    }
    Ok((setup, backend))
}

/// What the main loop does after an input event.
enum Step {
    Continue,
    Quit,
    Send(Value),
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if args.first().is_some_and(|arg| arg == "install") {
        let wait_pid = match args.as_slice() {
            [install, yes] if install == "install" && yes == "--yes" => None,
            [install, yes, wait, pid]
                if install == "install" && yes == "--yes" && wait == "--wait-pid" =>
            {
                Some(
                    pid.parse::<u32>()
                        .map_err(|_| io::Error::other("Invalid parent PID"))?,
                )
            }
            _ => {
                return Err(io::Error::other(
                    "Usage: daedalus install --yes [--wait-pid PID]",
                ));
            }
        };
        let installed = daedalus_tui::app::windows_install::install_current(wait_pid)
            .map_err(io::Error::other)?;
        println!("Installed Daedalus at {}", installed.display());
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "terminal-native" || arg == "shell")
    {
        let native_args = if args[0] == "shell" {
            args.as_slice()
        } else {
            &args[1..]
        };
        return daedalus_tui::app::native_terminal::cli(native_args).map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "agent") {
        return daedalus_tui::app::agent_tools::run(
            &args[1..],
            io::stdin().lock(),
            &mut io::stdout(),
        )
        .map_err(io::Error::other);
    }
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "job" | "jobs" | "jobctl"))
    {
        return daedalus_tui::app::public_cli::run_job(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "tma") {
        return daedalus_tui::app::public_cli::run_attach(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "update") {
        return daedalus_tui::app::update::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "memory") {
        return daedalus_tui::app::memory_status::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "route") {
        return daedalus_tui::app::route_cli::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "gateway-exec") {
        return daedalus_tui::app::gateway::run(&args[1..]).map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "telegram") {
        return daedalus_tui::app::telegram_service::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "diagnostics") {
        return daedalus_tui::app::diagnostics::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "setup")
        && args.iter().skip(1).any(|arg| arg == "--cli")
    {
        let options: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|arg| arg.as_str() != "--cli")
            .cloned()
            .collect();
        return daedalus_tui::app::wizard::run(
            &options,
            &mut io::stdin().lock(),
            &mut io::stdout(),
        )
        .map_err(io::Error::other);
    }
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "hook" | "goal"))
    {
        let result = daedalus_tui::app::hook_cli::run(
            &args[0],
            &args[1..],
            io::stdin().lock(),
            &mut io::stdout(),
        );
        if args[0] == "hook"
            && args.get(1).is_some_and(|arg| arg == "run")
            && args
                .windows(2)
                .any(|pair| pair[0] == "--event-kind" && pair[1] == "PreToolUse")
        {
            if let Err(error) = result {
                // Only an explicitly configured tool hook uses exit 2.
                // On Stop, exit 2 would request unwanted continuation.
                eprintln!("{error}");
                std::process::exit(2);
            }
            return Ok(());
        }
        return result.map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "backend") {
        if args == ["backend", "--stdio", "--demo"] {
            return daedalus_tui::app::demo::run(&mut io::stdin().lock(), &mut io::stdout().lock())
                .map_err(io::Error::other);
        }
        if args.len() == 7
            && args[1] == "--stdio"
            && args[2] == "--config-dir"
            && args[4] == "--first-run"
            && args[5] == "--workspace"
        {
            return daedalus_tui::app::backend::run_first_run(
                &mut io::stdin().lock(),
                &mut io::stdout().lock(),
                std::path::Path::new(&args[3]),
                std::path::Path::new(&args[6]),
            )
            .map_err(io::Error::other);
        }
        if args.len() != 4 || args[1] != "--stdio" || args[2] != "--config-dir" {
            return Err(io::Error::other(
                "Usage: daedalus backend --stdio --config-dir ABS | --demo",
            ));
        }
        return daedalus_tui::app::backend::run(
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
            std::path::Path::new(&args[3]),
        )
        .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "config") {
        return daedalus_tui::app::cli::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "checkpoint" | "ckpt"))
    {
        return daedalus_tui::app::session_cli::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "workspace-usage") {
        return daedalus_tui::app::workspace_api::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "inventory") {
        return daedalus_tui::app::inventory::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "agent-identity") {
        return daedalus_tui::app::agent_identity::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.first().is_some_and(|arg| arg == "terminal") {
        return daedalus_tui::app::terminal_cli::run(&args[1..], &mut io::stdout())
            .map_err(io::Error::other);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("daedalus {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Daedalus {} terminal control interface\nUsage: daedalus [tui|setup] [--demo] [--config-dir PATH] [--version]\n\nCommands: tui, setup, shell, terminal-native, agent, inventory, workspace-usage, terminal, checkpoint, route, job, hook, goal, telegram, memory, config, diagnostics, agent-identity, update.\nUse COMMAND --help for its options.\n\nTab or 0-9: sections; Ctrl+P: commands; /: search; ?: key bar; q: exit.\nWorkspaces: [ and ] switch Tree, Tiles, Wall, Cube and Focus; o orders by activity;\na attaches; w watches.\nInside an attached terminal, Ctrl+] then D detaches; the terminal keeps running.\nNO_COLOR=1 removes color; DAEDALUS_REDUCED_MOTION=1 stops animation;\nDAEDALUS_ICONS=ascii uses plain characters; DAEDALUS_BACKDROP=0 hides the background\nlight and artwork; DAEDALUS_SPLASH=0 skips the start-up splash;\nDAEDALUS_GRAPHICS=0 keeps artwork to text cells, =kitty always asks for pictures;\nDAEDALUS_SCREENSAVER=SECONDS sets the idle time before the screensaver (0: never); s toggles it.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let (mut setup, backend_args) = parse_args(args)?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "Interactive terminal required; use daedalus inventory --json for scripts",
        ));
    }
    let launch = daedalus_tui::app::startup::Launch::resolve(
        &backend_args,
        &std::env::current_dir()?,
        |name| std::env::var_os(name),
    )
    .map_err(io::Error::other)?;
    setup |= launch.first_run;
    let mut child = launch
        .command(std::env::current_exe()?.as_os_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mut host = Host { child, input };
    let (tx, rx) = mpsc::sync_channel(daedalus_tui::protocol::QUEUE_CAPACITY);
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        while let Ok(Some(value)) =
            daedalus_tui::protocol::read_frame(&mut reader, daedalus_tui::protocol::MAX_FRAME_BYTES)
        {
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let theme = theme::Theme::from_env();
    let mut ui = Ui {
        theme,
        still: theme.reduced_motion,
        splash: theme.splash.then(Splash::new),
        page: if setup { OVERVIEW } else { WORKSPACES },
        busy: true,
        status: "Discovering local configuration…".into(),
        preview_rows: 8,
        ..Ui::default()
    };
    let mut updater = update_ui::Controller::new(
        !backend_args.iter().any(|arg| arg == "--demo")
            && std::env::var("DAEDALUS_UPDATE_CHECK").as_deref() != Ok("0"),
    );
    let mut last = Instant::now();
    let mut suspended = false;
    // An action chosen during a background refresh waits for that refresh.
    let mut queued: Option<Value> = None;
    // Whether the first inventory has arrived; setup waits for it.
    let mut inventory = false;
    let mut terminal = ratatui::init();
    // Ask about pictures before anything else reads the input. The first
    // splash frame shows while the terminal answers. Only the splash uses one.
    if theme.backdrop && ui.splash.is_some() && graphics::wanted(|name| std::env::var(name).ok()) {
        terminal.draw(|f| view::draw(f, &mut ui))?;
        ui.kitty = graphics::probe(Duration::from_millis(1000));
        if ui.kitty.is_some() {
            graphics::transmit(&mut io::stdout())?;
        }
        // The wait for the answer and the picture's upload do not count
        // against the splash; its light starts spreading now.
        if let Some(splash) = ui.splash.as_mut() {
            splash.started = Instant::now();
        }
    }
    execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    let result = (|| -> io::Result<()> {
        'ui: loop {
            loop {
                match rx.try_recv() {
                    Ok(value) => {
                        if value["version"] != PROTOCOL {
                            return Err(io::Error::other("Unsupported backend protocol"));
                        }
                        match value["kind"].as_str().unwrap_or("") {
                            "state" => {
                                let state: Snapshot =
                                    serde_json::from_value(value).map_err(io::Error::other)?;
                                if ui.action_pending && state.page != ui.page {
                                    ui.navigate(state.page);
                                }
                                if ui.page != WORKSPACES {
                                    leave_focus(&mut ui);
                                }
                                let action_completed = ui.action_pending;
                                ui.update(state);
                                if let Some(splash) = ui.splash.as_mut() {
                                    splash.ready.get_or_insert_with(Instant::now);
                                }
                                inventory = true;
                                ui.busy = false;
                                ui.action_pending = false;
                                ui.refresh_status(action_completed);
                                last = Instant::now();
                                if let Some(request) = queued.take() {
                                    ui.action_pending = is_action(&request);
                                    host.send(request)?;
                                    ui.busy = true;
                                }
                            }
                            "dialog" => {
                                // A question is never hidden behind the splash
                                // or the screensaver.
                                end_splash(&mut ui);
                                wake(&mut ui);
                                let mut dialog = Dialog::from(value);
                                dialog.opened = Some(Instant::now());
                                ui.dialog = Some(dialog);
                                ui.options = None;
                            }
                            "attach" => open_embed(&mut ui, &value["plan"]),
                            "previews" => {
                                ui.preview_pending = false;
                                for item in value["items"].as_array().into_iter().flatten() {
                                    let field = |k: &str| item[k].as_str().unwrap_or("").to_owned();
                                    ui.previews.insert(
                                        field("id"),
                                        Preview::new(&field("text"), &field("error")),
                                    );
                                }
                            }
                            "external_done" => {
                                terminal = ratatui::init();
                                execute!(
                                    io::stdout(),
                                    EnableMouseCapture,
                                    EnableBracketedPaste,
                                    EnableFocusChange
                                )?;
                                suspended = false;
                            }
                            "error" => {
                                ui.busy = false;
                                ui.action_pending = false;
                                ui.preview_pending = false;
                                ui.status_error = true;
                                ui.status =
                                    value["message"].as_str().unwrap_or("Backend error").into();
                            }
                            _ => {}
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other(
                            "Backend stopped; no action outcome can be assumed",
                        ));
                    }
                }
            }
            if suspended {
                thread::sleep(Duration::from_millis(40));
                continue;
            }
            if let Some(embed) = ui.embed.as_mut() {
                embed.pump();
            }
            if ui
                .splash
                .as_mut()
                .is_some_and(|s| !s.advance(ui.theme.animated()))
            {
                end_splash(&mut ui);
            }
            // Setup, asked for or on a first run, opens a question, and a
            // question ends the splash at once; so it waits for the splash.
            if setup && inventory && setup_due(&ui) && queued.is_none() {
                setup = false;
                host.send(json!({"op":"action","page":OVERVIEW,"action":"setup"}))?;
                ui.busy = true;
                ui.action_pending = true;
            }
            if std::mem::take(&mut ui.updates_requested) {
                updater.open();
            }
            updater.poll(
                inventory
                    && ui.splash.is_none()
                    && ui.dialog.is_none()
                    && ui.options.is_none()
                    && ui.embed.is_none()
                    && !ui.searching
                    && !ui.busy,
            );
            if updater.should_quit() {
                // The backend remembers the view, then exits.
                if host
                    .send(json!({"op":"quit","view":view_state(&ui)}))
                    .is_ok()
                {
                    host.wait_exit(QUIT_WAIT);
                }
                break 'ui;
            }
            if updater.visible() {
                wake(&mut ui);
                ui.saver.last_input = Instant::now();
            }
            if !updater.visible()
                && may_rest(&ui)
                && ui.saver.due(ui.theme.screensaver, Instant::now())
            {
                ui.saver.since = Some(Instant::now());
            }
            terminal.draw(|f| {
                view::draw(f, &mut ui);
                let area = f.area();
                updater.draw(f, area, &ui.theme);
            })?;
            show_picture(&mut ui, &mut io::stdout())?;
            // Drain ready input before repainting. Mouse motion and fast typing
            // must not queue one full rendering interval per event.
            let mut input_started: Option<Instant> = None;
            for batch in 0..64 {
                if input_started.is_some_and(|at| at.elapsed() >= Duration::from_millis(4)) {
                    break;
                }
                if !event::poll(if batch == 0 {
                    frame_interval(&ui)
                } else {
                    Duration::ZERO
                })? {
                    break;
                }
                input_started.get_or_insert_with(Instant::now);
                if let Some(id) = ui.selected_terminal() {
                    ui.last_selected = id;
                }
                let event = event::read()?;
                if ui.splash.is_some() && skips_splash(&event) {
                    end_splash(&mut ui);
                    continue;
                }
                if matches!(event, Event::Resize(..)) {
                    ui.pane_resize = None;
                    ui.inline_screen = None;
                    if let Some(embed) = ui.embed.as_mut() {
                        embed.origin = None;
                    }
                }
                if matches!(event, Event::Resize(..) | Event::FocusGained) {
                    // The screen is redrawn from scratch; so is the picture. A
                    // multiplexer may blank a pane that lost focus, or repaint it
                    // at the same size, and the diff alone would never refill it.
                    repaint(&mut terminal)?;
                    if ui.kitty_shown.take().is_some() {
                        graphics::remove(&mut io::stdout(), false)?;
                    }
                }
                if updater.visible() {
                    match event {
                        Event::Key(key) => {
                            updater.key(key);
                        }
                        Event::Mouse(mouse) => {
                            updater.mouse(mouse);
                        }
                        _ => {}
                    }
                    continue;
                }
                if saver_input(&mut ui, &event) {
                    continue;
                }
                let step = match event {
                    Event::Paste(text) => paste(&mut ui, &text),
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        let update_shortcut = matches!(key.code, KeyCode::Char('u' | 'U'))
                            && (ui.embed.as_ref().is_some_and(|embed| embed.prefix)
                                || (key.modifiers.contains(KeyModifiers::CONTROL)
                                    && !ui
                                        .embed
                                        .as_ref()
                                        .is_some_and(|embed| embed.mode == Mode::Control)));
                        if shortcuts::key(&mut ui, key) {
                            Step::Continue
                        } else if update_shortcut && ui.dialog.is_none() && ui.options.is_none() {
                            if let Some(embed) = ui.embed.as_mut() {
                                embed.prefix = false;
                            }
                            updater.open();
                            Step::Continue
                        } else if let Some(dialog) = ui.dialog.take() {
                            suspended |= dialog_key(&mut ui, dialog, key, &mut host)?;
                            Step::Continue
                        } else if ui.options.is_some() {
                            options_key(&mut ui, key);
                            Step::Continue
                        } else {
                            key_event(&mut ui, key)
                        }
                    }
                    Event::Mouse(mouse) if ui.dialog.is_none() => mouse_event(&mut ui, mouse),
                    _ => Step::Continue,
                };
                match step {
                    Step::Quit => {
                        updater.request_exit();
                        break;
                    }
                    Step::Send(_) if ui.action_pending => {
                        ui.status = "Still working on the previous action".into();
                    }
                    Step::Send(request) => {
                        if ui.busy || ui.preview_pending {
                            queued = Some(request);
                        } else {
                            ui.action_pending = is_action(&request);
                            host.send(request)?;
                            ui.busy = true;
                        }
                    }
                    Step::Continue => {}
                }
            }
            if !ui.busy
                && ui.dialog.is_none()
                && !ui.preview_pending
                && queued.is_none()
                && last.elapsed() > Duration::from_secs(5)
            {
                host.send(json!({"op":"refresh"}))?;
                ui.busy = true;
                last = Instant::now();
            }
            // Panel sizes need no answer, so they never wait behind an action.
            if ui.dragging.is_none()
                && ui.dialog.is_none()
                && !ui.busy
                && ui
                    .sizes_changed
                    .is_some_and(|at| at.elapsed() > SIZES_SETTLE)
            {
                ui.sizes_changed = None;
                host.send(json!({"op":"view","view":view_state(&ui)}))?;
            }
            if queued.is_none() && previews_due(&ui) {
                host.send(json!({
                    "op":"previews",
                    "nodes":ui.preview_targets,
                    "rows":ui.preview_rows,
                }))?;
                ui.preview_pending = true;
                ui.last_preview = Some(Instant::now());
                ui.preview_sent = ui.preview_targets.clone();
            }
            if !ui.busy
                && !ui.preview_pending
                && let Some(request) = queued.take()
            {
                ui.action_pending = is_action(&request);
                host.send(request)?;
                ui.busy = true;
            }
        }
        Ok(())
    })();
    if let Some(mut embed) = ui.embed.take() {
        embed.detach();
    }
    if ui.kitty.is_some() {
        let _ = graphics::remove(&mut io::stdout(), true);
    }
    let _ = execute!(
        io::stdout(),
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    #[cfg(windows)]
    if result.is_ok()
        && let Some(installer) = updater.take_installer()
    {
        std::process::Command::new(installer)
            .arg(format!("/PARENT_PID={}", std::process::id()))
            .spawn()?;
    }
    result
}

/// Bounded preview reads: terminals on screen only (tiles, the Tree's
/// Details panel, or the other terminals of a tab in Focus), while nothing
/// else is pending.
fn previews_due(ui: &Ui) -> bool {
    let since = ui.last_preview.map(|t| t.elapsed());
    let changed = ui.preview_targets != ui.preview_sent;
    ui.page == WORKSPACES
        && ui.saver.since.is_none()
        && ui.dialog.is_none()
        && !ui.action_pending
        && !ui.busy
        && !ui.preview_pending
        && !ui.preview_targets.is_empty()
        && since.is_none_or(|t| t > PREVIEW_EVERY || (changed && t > PREVIEW_SETTLE))
}

/// The Workspaces view to remember, even while another page is open.
fn view_state(ui: &Ui) -> Value {
    let selected = if ui.page != WORKSPACES {
        ui.last_selected.clone()
    } else if ui.view == View::Focus {
        ui.focus_target.clone()
    } else {
        ui.selected_terminal().unwrap_or_default()
    };
    json!({
        "mode": ui.view.name(),
        "selected": selected,
        "sizes": ui.sizes,
        "backdrop": ui.look.name(),
        "saver": if ui.saver.enabled { "on" } else { "off" },
        "order": ui.order.name(),
        "details": switch(ui.prefs.details),
        "screen": switch(ui.prefs.screen),
        "pinned": switch(ui.prefs.pinned),
        "recent": switch(ui.prefs.recent),
        "badges": switch(ui.prefs.badges),
        "vitals": switch(ui.prefs.vitals),
        "motion": switch(ui.prefs.motion),
        "keys": switch(!ui.hide_keys),
    })
}

fn switch(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// A Workspaces request about one exact terminal.
fn terminal_request(ui: &Ui, action: &str, node: &str) -> Value {
    json!({"op":"action","page":WORKSPACES,"action":action,"node":node,"view":view_state(ui)})
}

fn open_embed(ui: &mut Ui, plan: &Value) {
    // An attach frame completes the backend action without a following state
    // frame. Stream connection progress belongs to Embed, not the action lock.
    // Release even on malformed plans or failed process startup so retry works.
    ui.busy = false;
    ui.action_pending = false;
    let Ok(plan) = serde_json::from_value::<Plan>(plan.clone()) else {
        ui.status = "Unexpected attach plan".into();
        ui.status_error = true;
        return;
    };
    if let Some(mut old) = ui.embed.take() {
        old.detach();
    }
    ui.inline_screen = None;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
    let (rows, cols) = view::focus_size(cols, rows);
    let node = plan.node.clone();
    let control = plan.mode == "control";
    match Embed::start(plan, rows, cols) {
        Ok(embed) => {
            let inline = ui.page == WORKSPACES && ui.view.grid();
            if !inline {
                if ui.view != View::Focus {
                    ui.previous = ui.view;
                }
                ui.view = View::Focus;
            }
            ui.page = WORKSPACES;
            if inline {
                ui.reveal(&node);
            }
            ui.paged_at = Some(Instant::now());
            if !node.is_empty() {
                ui.focus_target = node;
            }
            ui.embed = Some(embed);
            ui.status = if control {
                "Attaching: keys go to the terminal once it is verified."
            } else {
                "Watching: nothing you type reaches it."
            }
            .into();
            ui.status_error = false;
        }
        Err(error) => {
            ui.status = format!("Could not open the terminal stream: {error}");
            ui.status_error = true;
        }
    }
}

/// Forget what the screen holds and draw every cell again. `Terminal::clear`
/// asks the terminal where the cursor is, which can fail or stall; resizing to
/// the current size clears without asking.
fn repaint(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    let size = terminal.size()?;
    terminal.resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
}

/// Leave Focus: only the stream client stops; the terminal keeps running.
fn close_focus(ui: &mut Ui) {
    ui.pane_resize = None;
    ui.fullscreen_from = None;
    ui.zoomed = false;
    if let Some(mut embed) = ui.embed.take() {
        embed.detach();
        ui.status = "Detached. The terminal keeps running.".into();
        ui.status_error = false;
    }
    if ui.view == View::Focus {
        ui.view = if ui.previous == View::Focus {
            View::Tiles
        } else {
            ui.previous
        };
    }
    ui.inline_screen = None;
    ui.paged_at = Some(Instant::now());
    ui.takeover_armed = false;
    ui.last_preview = None;
    let target = ui.focus_target.clone();
    if !target.is_empty() {
        ui.reveal(&target);
    }
}

/// Leaving Workspaces stops a live view; nothing streams in the background.
/// Page of a number key in sidebar order: 1 is the first page, 0 the tenth.
fn digit_page(c: char) -> usize {
    (c as usize - '0' as usize + 9) % 10
}

fn leave_focus(ui: &mut Ui) {
    if ui.view == View::Focus || ui.embed.is_some() {
        close_focus(ui);
    }
}

fn paste(ui: &mut Ui, text: &str) -> Step {
    if ui.binding_search.is_some() {
        return Step::Continue;
    }

    // A form owns paste even when a controlled terminal remains behind it.
    if let Some(dialog) = ui.dialog.as_mut() {
        if dialog.mode == "input" {
            let clean: String = text
                .chars()
                .filter(|c| !c.is_control())
                .take(8192)
                .collect();
            dialog.input.push_str(&clean);
        }
        return Step::Continue;
    }
    if ui.dialog.is_none()
        && ui.options.is_none()
        && ui.page == WORKSPACES
        && ui.inline_screen.is_some()
        && let Some(embed) = ui.embed.as_mut()
        && embed.mode == Mode::Control
        && embed.live()
    {
        let text: String = text.chars().take(1 << 20).collect();
        embed.paste(&text);
        return Step::Continue;
    }
    if ui.embed.as_ref().is_some_and(|e| e.mode == Mode::Control) {
        ui.status = "Paste paused until the selected terminal is visible and connected.".into();
        return Step::Continue;
    }
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(8192)
        .collect();
    if ui.searching {
        ui.filter.push_str(&clean);
        ui.select(0, true);
        ui.select_tile(0);
    }
    Step::Continue
}

/// Returns true when a native program now owns the real terminal.
fn dialog_key(ui: &mut Ui, mut dialog: Dialog, key: KeyEvent, host: &mut Host) -> io::Result<bool> {
    #[cfg(windows)]
    if dialog.mode == "input" && clipboard::paste_key(key) {
        match clipboard::read() {
            Ok(text) => dialog
                .input
                .extend(text.chars().filter(|c| !c.is_control()).take(8192)),
            Err(message) => dialog.description = message.into(),
        }
        ui.dialog = Some(dialog);
        return Ok(false);
    }
    let mut reply = None;
    match key.code {
        KeyCode::Esc => reply = Some(Value::Null),
        KeyCode::Enter => {
            reply = Some(match dialog.mode.as_str() {
                "choose" => dialog
                    .options
                    .get(dialog.selected)
                    .map(|o| json!(o.0))
                    .unwrap_or(Value::Null),
                "input" => json!(dialog.input),
                "external" => json!("ready"),
                _ => json!("close"),
            })
        }
        KeyCode::Up => {
            dialog.selected = dialog.selected.saturating_sub(1);
            if dialog.mode != "choose" {
                dialog.scroll = dialog.scroll.saturating_sub(1);
            }
            dialog.moved = Some(Instant::now());
        }
        KeyCode::Down => {
            dialog.selected = (dialog.selected + 1).min(dialog.options.len().saturating_sub(1));
            if dialog.mode != "choose" {
                dialog.scroll = dialog.scroll.saturating_add(1);
            }
            dialog.moved = Some(Instant::now());
        }
        KeyCode::PageDown => dialog.scroll = dialog.scroll.saturating_add(10),
        KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(10),
        KeyCode::Backspace if dialog.mode == "input" => {
            dialog.input.pop();
        }
        KeyCode::Char(c)
            if dialog.mode == "input"
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && dialog.input.len() < 32768 =>
        {
            dialog.input.push(c);
        }
        _ => {}
    }
    let Some(value) = reply else {
        ui.dialog = Some(dialog);
        return Ok(false);
    };
    let external = dialog.mode == "external" && value == "ready";
    if external {
        // The native client takes the real terminal; stop the embedded view first.
        leave_focus(ui);
        execute!(
            io::stdout(),
            DisableFocusChange,
            DisableMouseCapture,
            DisableBracketedPaste
        )?;
        ratatui::restore();
    }
    host.send(json!({"op":"reply","id":dialog.id,"value":value}))?;
    Ok(external)
}

/// The key after Ctrl+] while a terminal is open in Focus.
fn prefix_command(ui: &mut Ui, key: KeyEvent) -> Step {
    let armed = std::mem::take(&mut ui.takeover_armed);
    match ui.embed.as_mut() {
        Some(embed) => embed.prefix = false,
        None => return Step::Continue,
    }
    if matches!(key.code, KeyCode::Char('[' | ']')) {
        return workspace_key(ui, key).unwrap_or(Step::Continue);
    }
    if ui.view == View::Cube && matches!(key.code, KeyCode::Char('<' | '>' | '+' | '-' | '_' | '='))
    {
        resize_key(ui, key);
        return Step::Continue;
    }
    if ui.view.grid() {
        if matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Char('[' | ']')
        ) {
            ui.detach_inline();
            return workspace_key(ui, key).unwrap_or(Step::Continue);
        }
        if matches!(key.code, KeyCode::Char('z' | 'Z')) {
            return switch_view(ui, View::Focus);
        }
    }
    if let Some(step) = focus_resize_key(ui, key) {
        return step;
    }
    if matches!(key.code, KeyCode::Char('n' | 'N')) {
        return quick_action(ui, "terminal.new");
    }
    if matches!(key.code, KeyCode::Char('k' | 'K')) {
        return open_switcher(ui);
    }
    if matches!(key.code, KeyCode::Char('g' | 'G')) {
        return quick_action(ui, "telegram.route");
    }
    if matches!(key.code, KeyCode::Char('w' | 'W' | 'b' | 'B')) {
        return wall_preset(
            ui,
            if matches!(key.code, KeyCode::Char('b' | 'B')) {
                "wall.working"
            } else {
                "wall.all"
            },
        );
    }
    match key.code {
        KeyCode::Char('d' | 'D' | 'q' | 'Q') | KeyCode::Esc => {
            close_focus(ui);
            return Step::Continue;
        }
        KeyCode::Char('m' | 'M') => {
            return match ui.selected_terminal() {
                Some(node) => Step::Send(terminal_request(ui, &node, &node)),
                None => Step::Continue,
            };
        }
        _ => {}
    }
    match key.code {
        KeyCode::Char('z' | 'Z') => {
            toggle_zoom(ui);
            return Step::Continue;
        }
        KeyCode::Left => return move_pane(ui, -1, 0),
        KeyCode::Right => return move_pane(ui, 1, 0),
        KeyCode::Up => return move_pane(ui, 0, -1),
        KeyCode::Down => return move_pane(ui, 0, 1),
        _ => {}
    }
    let Some(embed) = ui.embed.as_mut() else {
        return Step::Continue;
    };
    match key.code {
        KeyCode::Char('o' | 'O') => embed.observe(),
        KeyCode::Char('a' | 'A' | 'c' | 'C') => embed.request_control(),
        KeyCode::Char('r' | 'R') => embed.resync(),
        KeyCode::Char('f' | 'F') => {
            let fit = !embed.fit;
            embed.set_fit(fit);
        }
        KeyCode::Char('t' | 'T') if !embed.is_tmux() => {
            if armed {
                embed.takeover();
            } else {
                ui.takeover_armed = true;
                embed.notice =
                    "Take Herdr's attach slot from its current holder? Ctrl+] then T again confirms."
                        .into();
            }
        }
        // Ctrl+] twice sends one Ctrl+] to the terminal.
        _ if embed::is_prefix(&key) => embed.send(&[0x1d]),
        _ => {}
    }
    Step::Continue
}

fn key_event(ui: &mut Ui, key: KeyEvent) -> Step {
    if shortcuts::key(ui, key) {
        return Step::Continue;
    }

    if key.code == KeyCode::F(11) && ui.page == WORKSPACES {
        if let Some(embed) = ui.embed.as_mut() {
            embed.prefix = false;
        }
        return toggle_fullscreen(ui);
    }
    if ui.pane_resize.is_some() && key.code == KeyCode::Esc {
        ui.pane_resize = None;
        ui.status = "Pane resize cancelled".into();
        return Step::Continue;
    }
    // A hidden grid terminal cannot consume navigation. Keep text blocked, but
    // allow the user to select another face, enlarge it or leave input mode.
    if ui.page == WORKSPACES
        && ui.view.grid()
        && ui.inline_screen.is_none()
        && ui
            .embed
            .as_ref()
            .is_some_and(|e| e.mode == Mode::Control && !e.prefix)
    {
        if key.code == KeyCode::Esc {
            close_focus(ui);
            return Step::Continue;
        }
        if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return open_switcher(ui);
        }
        if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && (matches!(
                key.code,
                KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Char('[' | ']')
            ) || (ui.view == View::Cube
                && matches!(key.code, KeyCode::Char('<' | '>' | '+' | '-' | '_' | '='))))
        {
            return prefix_command(ui, key);
        }
    }
    // A visible attached terminal gets every key except the Ctrl+] prefix.
    if ui.page == WORKSPACES
        && let Some(embed) = ui.embed.as_mut()
    {
        if embed.prefix {
            return prefix_command(ui, key);
        }
        if embed::is_prefix(&key) {
            embed.prefix = true;
            return Step::Continue;
        }
        if embed.mode == Mode::Control {
            if embed.live() && ui.inline_screen.is_some() {
                embed.key(key);
            } else {
                ui.status = "Input paused until the selected terminal is visible and connected. Ctrl+] then D detaches.".into();
            }
            return Step::Continue;
        }
    }
    if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return open_switcher(ui);
    }
    if ui.switching {
        if key.code == KeyCode::Esc {
            ui.switching = false;
            ui.searching = false;
            ui.filter.clear();
            ui.select(0, true);
            return Step::Continue;
        }
        if key.code == KeyCode::Enter {
            return choose_switcher(ui, None);
        }
        if key.modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c @ '1'..='9') = key.code
        {
            return choose_switcher(ui, Some((c as usize) - ('1' as usize)));
        }
    }
    let tiles = ui.page == WORKSPACES && ui.view.grid();
    if ui.searching {
        let row = ui.tile_columns.max(1) as isize;
        match key.code {
            KeyCode::Enter => ui.searching = false,
            KeyCode::Esc => {
                ui.searching = false;
                ui.filter.clear();
                ui.select(0, true);
                ui.select_tile(0);
            }
            KeyCode::Up if tiles => ui.move_tile(-row),
            KeyCode::Down if tiles => ui.move_tile(row),
            KeyCode::Up => ui.move_by(-1),
            KeyCode::Down => ui.move_by(1),
            KeyCode::Backspace => {
                ui.filter.pop();
                ui.select(0, true);
                ui.select_tile(0);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                ui.filter.push(c);
                ui.select(0, true);
                ui.select_tile(0);
            }
            _ => {}
        }
        return Step::Continue;
    }
    if ui.page == WORKSPACES && !key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.code == KeyCode::Char('W') {
            return wall_preset(ui, "wall.all");
        }
        if key.code == KeyCode::Char('B') {
            return wall_preset(ui, "wall.working");
        }
    }
    let pages = ui.page_count();
    let workspaces = ui.page == WORKSPACES;
    match key.code {
        KeyCode::Char('q') => return Step::Quit,
        KeyCode::Char('?') => {
            change_option(ui, Item::Keys, true);
            return Step::Continue;
        }
        KeyCode::Char(',') if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            open_options(ui);
            return Step::Continue;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Step::Quit,
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            leave_focus(ui);
            ui.navigate(COMMANDS);
            ui.searching = true;
            return Step::Continue;
        }
        KeyCode::Char('/') if ui.view != View::Focus || !workspaces => {
            ui.searching = true;
            return Step::Continue;
        }
        KeyCode::Esc if workspaces && (ui.view == View::Focus || ui.embed.is_some()) => {
            close_focus(ui);
            return Step::Continue;
        }
        KeyCode::Esc => {
            ui.filter.clear();
            ui.select(0, true);
            ui.select_tile(ui.tile);
            return Step::Continue;
        }
        KeyCode::Tab => {
            leave_focus(ui);
            ui.navigate((ui.page + 1) % pages);
            return Step::Continue;
        }
        KeyCode::BackTab => {
            leave_focus(ui);
            ui.navigate((ui.page + pages - 1) % pages);
            return Step::Continue;
        }
        // Number keys follow the sidebar; 0 is the tenth page.
        KeyCode::Char(c @ '0'..='9') if digit_page(c) < pages.min(COMMANDS) => {
            leave_focus(ui);
            ui.navigate(digit_page(c));
            return Step::Continue;
        }
        KeyCode::Char('r') if !ui.busy => return Step::Send(json!({"op":"refresh"})),
        _ => {}
    }
    if let Some(step) = focus_resize_key(ui, key) {
        return step;
    }
    if resize_key(ui, key) {
        return Step::Continue;
    }
    if workspaces && let Some(step) = workspace_key(ui, key) {
        return step;
    }
    match key.code {
        KeyCode::Up => ui.move_by(-1),
        KeyCode::Down => ui.move_by(1),
        KeyCode::PageUp => ui.move_by(-10),
        KeyCode::PageDown => ui.move_by(10),
        KeyCode::Home => ui.select(0, true),
        KeyCode::End => ui.select(usize::MAX, false),
        KeyCode::Right | KeyCode::Left if workspaces => fold(ui, key.code == KeyCode::Right),
        KeyCode::Char('E') if workspaces => ui.collapsed.clear(),
        KeyCode::Char('C') if workspaces => {
            ui.collapsed = ui
                .snapshot
                .tree
                .iter()
                .filter(|n| n.children)
                .map(|n| n.id.clone())
                .collect();
            ui.select(0, true);
        }
        KeyCode::Enter if !ui.action_pending => {
            if let Some(request) = activate(ui) {
                return Step::Send(request);
            }
        }
        _ => {}
    }
    Step::Continue
}

/// Keys shared by Tree, Tiles and Focus. None falls through to list keys.
/// Cube navigation never forwards input to the harness.
fn cube_key(ui: &mut Ui, key: KeyEvent) -> Option<Step> {
    let delta = match key.code {
        KeyCode::Left | KeyCode::Up | KeyCode::PageUp | KeyCode::Home => -1,
        KeyCode::Right | KeyCode::Down | KeyCode::PageDown | KeyCode::End => 1,
        _ => return None,
    };
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
    {
        return None;
    }
    let tiles = ui.tiles();
    let groups = view::cube_groups(ui, &tiles);
    let Some(group_index) = groups.iter().position(|g| g.contains(&ui.tile)) else {
        return Some(Step::Continue);
    };
    let current_key = view::cube_workspace(ui, &tiles[ui.tile]).0;
    let workspace_keys: Vec<_> = groups
        .iter()
        .map(|g| view::cube_workspace(ui, &tiles[g[0]]).0)
        .collect();
    ui.cube_positions
        .retain(|identity, _| workspace_keys.contains(identity));
    ui.cube_positions.insert(
        current_key,
        (tiles[ui.tile].id.clone(), ui.cube_anchor.clone()),
    );
    ui.cube_motion = match key.code {
        KeyCode::Left | KeyCode::Right => (delta as i16, 0),
        _ => (0, delta as i16),
    };
    ui.clicked = None;
    if matches!(key.code, KeyCode::Home | KeyCode::End) {
        let members = &groups[group_index];
        ui.cube_anchor = None;
        ui.select_tile(if key.code == KeyCode::Home {
            members[0]
        } else {
            members[members.len() - 1]
        });
        return Some(Step::Continue);
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if (alt && matches!(key.code, KeyCode::Up | KeyCode::Down))
        || matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
    {
        let next = (group_index as isize + delta).rem_euclid(groups.len() as isize) as usize;
        let members = &groups[next];
        let key = &workspace_keys[next];
        let remembered = ui.cube_positions.get(key).cloned();
        let selected = remembered
            .as_ref()
            .and_then(|(id, _)| members.iter().find(|i| tiles[**i].id == *id))
            .copied()
            .unwrap_or(members[0]);
        ui.cube_anchor = remembered
            .and_then(|(_, anchor)| anchor)
            .filter(|id| members.iter().any(|i| tiles[*i].id == *id));
        ui.select_tile(selected);
        ui.status = format!(
            "Cube {} of {} · returned to your last terminal",
            next + 1,
            groups.len()
        );
    } else {
        let members = &groups[group_index];
        if alt {
            if ui.cube_anchor.is_none() {
                ui.cube_anchor = Some(tiles[ui.tile].id.clone());
            }
        } else if let Some(anchor) = ui.cube_anchor.take()
            && let Some(index) = members.iter().find(|i| tiles[**i].id == anchor)
        {
            ui.tile = *index;
        }
        let position = members.iter().position(|i| *i == ui.tile).unwrap_or(0);
        let step = if !alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            delta * 2
        } else {
            delta
        };
        let next = (position as isize + step).rem_euclid(members.len() as isize) as usize;
        ui.select_tile(members[next]);
        ui.status = if alt {
            "Front terminal changed; cube orientation held."
        } else {
            "Rotated within this workspace."
        }
        .into();
    }
    Some(Step::Continue)
}

fn workspace_key(ui: &mut Ui, key: KeyEvent) -> Option<Step> {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('n') if plain => {
            return Some(Step::Send(
                json!({"op":"action","page":WORKSPACES,"action":"project.new"}),
            ));
        }
        KeyCode::Char(c @ ('[' | ']')) if plain => {
            let i = View::ALL.iter().position(|v| *v == ui.view).unwrap_or(0) as isize;
            let step = if c == ']' { 1 } else { -1 };
            let target = View::ALL[(i + step).rem_euclid(View::ALL.len() as isize) as usize];
            return Some(switch_view(ui, target));
        }
        KeyCode::Char('z') if plain => {
            let target = if ui.view == View::Focus {
                ui.previous
            } else {
                View::Focus
            };
            return Some(switch_view(ui, target));
        }
        KeyCode::Char('P') if ui.view.grid() => {
            ui.working_only = false;
            ui.pinned_only = !ui.pinned_only;
            ui.select_tile(0);
            ui.last_preview = None;
            return Some(Step::Continue);
        }
        KeyCode::Char('L') if plain => {
            // Save from any view, so the layout records Tiles or Focus too.
            return Some(Step::Send(json!({
                "op":"action","page":WORKSPACES,"action":"layout.save","view":view_state(ui),
            })));
        }
        KeyCode::Char('m') if plain => {
            return Some(match ui.selected_terminal() {
                Some(node) => Step::Send(terminal_request(ui, &node, &node)),
                None => Step::Continue,
            });
        }
        KeyCode::Char('o') if plain && ui.view != View::Focus => {
            toggle_order(ui);
            return Some(Step::Continue);
        }
        KeyCode::Char('u') if plain && ui.view.grid() => {
            // Forget a pin whose terminal is gone; live pins toggle with p.
            let key = ui
                .tiles()
                .get(ui.tile)
                .filter(|t| t.id.is_empty())
                .and_then(|t| t.pin)
                .and_then(|p| ui.snapshot.workspace.pins.get(p))
                .map(|p| p.key.clone());
            return Some(match key {
                Some(key) => Step::Send(json!({
                    "op":"action","page":WORKSPACES,"action":format!("unpin:{key}"),
                    "view":view_state(ui),
                })),
                None => Step::Continue,
            });
        }
        KeyCode::Char(c) if plain => {
            let quick = ui
                .snapshot
                .terminal_actions
                .iter()
                .find(|a| a.quick.chars().eq([c]))
                .map(|a| a.id.clone());
            if let Some(action) = quick {
                return Some(quick_action(ui, &action));
            }
        }
        _ => {}
    }
    match ui.view {
        View::Tree => None,
        View::Tiles | View::Wall | View::Cube => {
            let row = ui.tile_columns.max(1) as isize;
            if matches!(ui.view, View::Tiles | View::Wall)
                && key.modifiers.contains(KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Up | KeyCode::Down)
            {
                return cube_key(ui, key);
            }
            if ui.view == View::Tiles
                && matches!(
                    key.code,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                )
            {
                let groups = view::cube_groups(ui, &ui.tiles());
                if let Some(members) = groups.iter().find(|g| g.contains(&ui.tile)) {
                    let at = members.iter().position(|i| *i == ui.tile).unwrap_or(0) as isize;
                    let page = ui.tiles_per_page.max(1) as isize;
                    let next = match key.code {
                        KeyCode::Home => 0,
                        KeyCode::End => members.len() as isize - 1,
                        KeyCode::PageUp => at - page,
                        KeyCode::PageDown => at + page,
                        KeyCode::Left | KeyCode::Up => at - 1,
                        _ => at + 1,
                    }
                    .clamp(0, members.len() as isize - 1) as usize;
                    ui.select_tile(members[next]);
                }
                return Some(Step::Continue);
            }
            if ui.view == View::Cube
                && let Some(step) = cube_key(ui, key)
            {
                return Some(step);
            }
            let page = ui.tiles_per_page.max(1) as isize;
            match key.code {
                KeyCode::Left => ui.move_tile(-1),
                KeyCode::Right => ui.move_tile(1),
                KeyCode::Up => ui.move_tile(-row),
                KeyCode::Down => ui.move_tile(row),
                KeyCode::PageUp => ui.move_tile(-page),
                KeyCode::PageDown => ui.move_tile(page),
                KeyCode::Home => ui.select_tile(0),
                KeyCode::End => ui.select_tile(usize::MAX),
                // The same menu as in Tree; double-click attaches, z opens Focus.
                KeyCode::Enter => {
                    return Some(match ui.selected_terminal() {
                        Some(node) => Step::Send(terminal_request(ui, &node, &node)),
                        None => switch_view(ui, View::Focus),
                    });
                }
                _ => return None,
            }
            Some(Step::Continue)
        }
        View::Focus => Some(match key.code {
            // Not yet connected: watch first; watching: Enter asks for input.
            KeyCode::Enter if ui.embed.as_ref().is_some_and(|e| e.live()) => {
                quick_action(ui, "terminal.attach")
            }
            KeyCode::Enter => quick_action(ui, "terminal.watch"),
            // Arrows move to the terminal beside this one in its tab.
            KeyCode::Left => move_pane(ui, -1, 0),
            KeyCode::Right => move_pane(ui, 1, 0),
            KeyCode::Up => move_pane(ui, 0, -1),
            KeyCode::Down => move_pane(ui, 0, 1),
            KeyCode::Char('Z') => {
                toggle_zoom(ui);
                Step::Continue
            }
            _ => Step::Continue,
        }),
    }
}

/// Focus the terminal beside the current one in its tab.
fn move_pane(ui: &mut Ui, dx: i32, dy: i32) -> Step {
    let current = ui
        .terminal_node(&ui.focus_target)
        .map(|n| n.id.clone())
        .unwrap_or_default();
    if ui.tab_layout(&current).is_none() {
        ui.status = "This terminal is alone in its tab".into();
        ui.status_error = false;
        return Step::Continue;
    }
    match ui.neighbor(&current, dx, dy) {
        Some(id) => focus_pane(ui, &id),
        None => {
            ui.status = "No terminal that way in this tab".into();
            ui.status_error = false;
            Step::Continue
        }
    }
}

/// Move Focus to another terminal of the tab. The old one is detached and
/// keeps running; the new one starts read-only, like any Focus.
fn focus_pane(ui: &mut Ui, id: &str) -> Step {
    if ui.terminal_node(id).is_none() {
        ui.status = "Terminal disappeared; refresh and select it again".into();
        ui.status_error = true;
        return Step::Continue;
    }
    ui.pane_resize = None;
    if let Some(mut old) = ui.embed.take() {
        old.detach();
    }
    ui.reveal(id);
    ui.takeover_armed = false;
    ui.last_preview = None;
    ui.zoomed = false;
    ui.fullscreen_from = None;
    quick_action(ui, "terminal.watch")
}

/// One terminal alone, or its whole tab.
fn toggle_zoom(ui: &mut Ui) {
    let current = ui
        .terminal_node(&ui.focus_target)
        .map(|n| n.id.clone())
        .unwrap_or_default();
    if ui.tab_layout(&current).is_none() {
        ui.status = "This terminal is alone in its tab".into();
        ui.status_error = false;
        return;
    }
    ui.zoomed = !ui.zoomed;
    if !ui.zoomed {
        ui.fullscreen_from = None;
    }
    ui.last_preview = None;
    ui.status_error = false;
    ui.status = if ui.zoomed {
        "Zoomed · Z shows the whole tab".into()
    } else {
        "Whole tab · arrows move between its terminals · Z zooms".into()
    };
}

fn switch_view(ui: &mut Ui, target: View) -> Step {
    if target != ui.view {
        ui.detach_inline();
    }
    if ui.view == View::Focus && target != View::Focus {
        ui.previous = target;
        close_focus(ui);
        return Step::Continue;
    }
    if target == View::Focus && ui.selected_terminal().is_none() {
        ui.status = "Select a terminal to focus".into();
        ui.status_error = true;
        return Step::Continue;
    }
    ui.set_view(target);
    ui.last_preview = None;
    // Remembered once it settles, like a panel size.
    ui.sizes_changed = Some(Instant::now());
    if target == View::Focus {
        // Focus starts read-only; attaching for input is a separate choice.
        return quick_action(ui, "terminal.watch");
    }
    Step::Continue
}

/// F11 fills the screen with the selected terminal. The same key leaves it.
/// From Wall, Tiles or Cube it opens Focus zoomed, and returns there afterwards.
/// From Focus it only zooms, and the second press restores the whole tab.
fn toggle_fullscreen(ui: &mut Ui) -> Step {
    if let Some(origin) = ui.fullscreen_from.take() {
        ui.zoomed = false;
        ui.last_preview = None;
        if origin != View::Focus {
            close_focus(ui);
        }
        ui.status = "Left fullscreen".into();
        ui.status_error = false;
        return Step::Continue;
    }
    if ui.selected_terminal().is_none() {
        ui.status = "Select a terminal to fill the screen".into();
        ui.status_error = true;
        return Step::Continue;
    }
    let origin = ui.view;
    let step = if origin == View::Focus {
        Step::Continue
    } else {
        switch_view(ui, View::Focus)
    };
    if ui.view != View::Focus {
        return step;
    }
    ui.fullscreen_from = Some(origin);
    ui.zoomed = true;
    ui.last_preview = None;
    ui.status = "Fullscreen · F11 returns".into();
    ui.status_error = false;
    step
}

fn wall_preset(ui: &mut Ui, scope: &str) -> Step {
    leave_focus(ui);
    ui.switching = false;
    ui.searching = false;
    ui.filter.clear();
    ui.working_only = scope == "wall.working";
    ui.pinned_only = scope == "wall.pinned";
    ui.order = Order::Activity;
    ui.view = View::Wall;
    ui.select_tile(0);
    ui.last_preview = None;
    Step::Continue
}

fn open_switcher(ui: &mut Ui) -> Step {
    leave_focus(ui);
    ui.navigate(WORKSPACES);
    ui.view = View::Tree;
    ui.switching = true;
    ui.searching = true;
    ui.filter.clear();
    ui.select(0, true);
    ui.status =
        "Find a workspace, tab, pane or agent · Enter watches · Alt+1–9 opens a result".into();
    ui.status_error = false;
    Step::Continue
}
fn choose_switcher(ui: &mut Ui, rank: Option<usize>) -> Step {
    let node = match rank {
        Some(rank) => ui
            .rows()
            .into_iter()
            .filter(|r| r.kind == Kind::Node)
            .nth(rank)
            .map(|r| r.id),
        None => ui.selected_terminal(),
    };
    let Some(node) = node else {
        ui.status = "No matching terminal".into();
        return Step::Continue;
    };
    ui.switching = false;
    ui.searching = false;
    ui.filter.clear();
    ui.reveal(&node);
    Step::Send(terminal_request(ui, "terminal.watch", &node))
}

fn quick_action(ui: &mut Ui, action: &str) -> Step {
    let Some(node) = ui.selected_terminal() else {
        ui.status = "Select a terminal first".into();
        ui.status_error = true;
        return Step::Continue;
    };
    if ui.view == View::Cube
        && action == "terminal.attach"
        && !ui
            .tile_hits
            .first()
            .is_some_and(|(rect, _)| rect.width >= 24 && rect.height >= 7)
    {
        ui.status = "Cube face too small to type. Enlarge it or select another terminal.".into();
        ui.status_error = false;
        return Step::Continue;
    }
    if let Some(embed) = ui.embed.as_ref()
        && embed.plan.node == node
        && embed.phase == embed::Phase::Connecting
        && matches!(action, "terminal.attach" | "terminal.watch")
    {
        // Pressing again would only start over; the status says what it waits on.
        ui.status = "Still connecting; Esc cancels".into();
        ui.status_error = false;
        return Step::Continue;
    }
    if let Some(embed) = ui.embed.as_mut()
        && embed.plan.node == node
        && embed.live()
    {
        // Already streaming this terminal: switch input mode in place.
        match action {
            "terminal.attach" => {
                embed.request_control();
                ui.status_error = false;
                return Step::Continue;
            }
            "terminal.watch" => {
                embed.observe();
                ui.status_error = false;
                return Step::Continue;
            }
            _ => {}
        }
    }
    ui.status = "Opening…".into();
    ui.status_error = false;
    Step::Send(terminal_request(ui, action, &node))
}

fn mouse_event(ui: &mut Ui, mouse: MouseEvent) -> Step {
    if ui.binding_search.is_some() {
        return Step::Continue;
    }

    ui.pointer = Some((mouse.column, mouse.row));
    if ui.options.is_some() {
        options_mouse(ui, mouse);
        return Step::Continue;
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    if ui.focus_tree_area.is_some_and(|r| r.contains(position)) {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                ui.focus_tree_offset = ui.focus_tree_offset.saturating_add(3)
            }
            MouseEventKind::ScrollUp => {
                ui.focus_tree_offset = ui.focus_tree_offset.saturating_sub(3)
            }
            MouseEventKind::Down(event::MouseButton::Left) => {
                if let Some((_, id)) = ui
                    .focus_tree_toggles
                    .iter()
                    .find(|(r, _)| r.contains(position))
                    .cloned()
                {
                    if !ui.collapsed.remove(&id) {
                        ui.collapsed.insert(id);
                    }
                } else if let Some((_, id, branch)) = ui
                    .focus_tree_hits
                    .iter()
                    .find(|(r, _, _)| r.contains(position))
                    .cloned()
                {
                    if branch {
                        if !ui.collapsed.remove(&id) {
                            ui.collapsed.insert(id);
                        }
                    } else if let Some(target) = ui.terminal_node(&id).map(|n| n.id.clone())
                        && ui
                            .terminal_node(&ui.focus_target)
                            .is_none_or(|n| n.id != target)
                    {
                        return focus_pane(ui, &target);
                    }
                }
            }
            _ => {}
        }
        return Step::Continue;
    }
    if let Some(step) = focus_resize_mouse(ui, mouse) {
        return step;
    }
    if ui.view == View::Cube
        && (ui.dragging.is_some() || ui.handles.iter().any(|h| h.area.contains(position)))
        && let Some(step) = border_mouse(ui, mouse, position)
    {
        return step;
    }
    if ui.inline_screen.is_some()
        && let Some(embed) = ui.embed.as_mut()
        && embed.mode == Mode::Control
        && embed.mouse(mouse)
    {
        return Step::Continue;
    }
    let p = ratatui::layout::Position::new(mouse.column, mouse.row);
    if let Some(step) = border_mouse(ui, mouse, p) {
        return step;
    }
    let tiles = ui.page == WORKSPACES && ui.view.grid();
    let row = ui.tile_columns.max(1) as isize;
    match mouse.kind {
        MouseEventKind::ScrollDown if tiles && ui.view == View::Cube => {
            cube_key(
                ui,
                KeyEvent::new(
                    if mouse.modifiers.contains(KeyModifiers::ALT) {
                        KeyCode::Down
                    } else {
                        KeyCode::Right
                    },
                    mouse.modifiers,
                ),
            );
        }
        MouseEventKind::ScrollUp if tiles && ui.view == View::Cube => {
            cube_key(
                ui,
                KeyEvent::new(
                    if mouse.modifiers.contains(KeyModifiers::ALT) {
                        KeyCode::Up
                    } else {
                        KeyCode::Left
                    },
                    mouse.modifiers,
                ),
            );
        }
        MouseEventKind::ScrollDown if tiles => ui.move_tile(row),
        MouseEventKind::ScrollUp if tiles => ui.move_tile(-row),
        MouseEventKind::ScrollDown if ui.view != View::Focus => ui.move_by(3),
        MouseEventKind::ScrollUp if ui.view != View::Focus => ui.move_by(-3),
        MouseEventKind::Down(event::MouseButton::Left) => {
            if let Some(&(_, page)) = ui.nav_hits.iter().find(|(r, _)| r.contains(p)) {
                leave_focus(ui);
                ui.navigate(page);
            } else if let Some((_, id)) = ui.side_hits.iter().find(|(r, _)| r.contains(p)) {
                let id = id.clone();
                return jump_to(ui, &id);
            } else if let Some(&(_, view)) = ui.view_hits.iter().find(|(r, _)| r.contains(p)) {
                return switch_view(ui, view);
            } else if let Some((_, action)) = ui.quick_hits.iter().find(|(r, _)| r.contains(p)) {
                let action = action.clone();
                if let Some(index) = action
                    .strip_prefix("workspace:")
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    ui.select_tile(index);
                    ui.clicked = None;
                    return Step::Continue;
                }
                return match action.as_str() {
                    "wall.all" | "wall.working" | "wall.pinned" => wall_preset(ui, &action),
                    "detach" => {
                        close_focus(ui);
                        Step::Continue
                    }
                    "sidebar" => {
                        toggle_sidebar(ui);
                        Step::Continue
                    }
                    "options" => {
                        open_options(ui);
                        Step::Continue
                    }
                    "focus.resize.hint" => {
                        ui.status = "Drag a divider, or Alt+arrows to resize. While attached: Ctrl+] then Alt+arrow.".into();
                        Step::Continue
                    }
                    "more" => match ui.selected_terminal() {
                        Some(node) => Step::Send(terminal_request(ui, &node, &node)),
                        None => Step::Continue,
                    },
                    _ => quick_action(ui, &action),
                };
            } else if let Some((_, id)) = ui
                .pane_hits
                .iter()
                .find(|(r, _)| ui.view != View::Cube && r.contains(p))
            {
                // Another terminal of the tab shown in Focus: move to it.
                let id = id.clone();
                if ui.view.grid() {
                    ui.reveal(&id);
                    return quick_action(ui, "terminal.attach");
                }
                let current = ui.terminal_node(&ui.focus_target).map(|n| n.id.clone());
                if current.as_deref() != Some(id.as_str()) {
                    return focus_pane(ui, &id);
                }
            } else if let Some(&(_, index)) = ui.tile_hits.iter().find(|(r, _)| r.contains(p)) {
                if ui.view == View::Cube && index != ui.tile {
                    ui.cube_anchor = None;
                }
                ui.select_tile(index);
                if ui.view == View::Cube {
                    ui.clicked = None;
                    return quick_action(ui, "terminal.attach");
                }
                let again = ui
                    .clicked
                    .is_some_and(|(i, at)| i == index && at.elapsed() < Duration::from_millis(500));
                ui.clicked = Some((index, Instant::now()));
                if again {
                    return quick_action(ui, "terminal.attach");
                }
            } else if let Some(&(_, row)) = ui.list_hits.iter().find(|(r, _)| r.contains(p)) {
                ui.select(row, true);
            } else if ui.embed.is_some() {
                close_focus(ui);
            }
        }
        _ => {}
    }
    Step::Continue
}

/// The mouse while Options is open: a click on a row changes it, a click
/// outside closes, the wheel moves the choice.
fn options_mouse(ui: &mut Ui, mouse: MouseEvent) {
    let p = ratatui::layout::Position::new(mouse.column, mouse.row);
    let hit = ui
        .option_hits
        .iter()
        .find(|(r, _)| r.contains(p))
        .map(|&(_, i)| i);
    let Some(menu) = ui.options.as_mut() else {
        return;
    };
    let last = Item::ALL.len() - 1;
    match mouse.kind {
        MouseEventKind::ScrollDown => menu.selected = (menu.selected + 1).min(last),
        MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(1),
        MouseEventKind::Down(event::MouseButton::Left) => match hit {
            Some(i) => {
                menu.selected = i;
                change_option(ui, Item::ALL[i], true);
            }
            None if !ui.option_panel.contains(p) => ui.options = None,
            None => {}
        },
        _ => {}
    }
}

/// Borders move like pane borders in tmux or Herdr: press on one, drag,
/// release. A double click puts it back. None when the mouse is elsewhere.
/// Submit a single native mutation. The backend rechecks the captured complete
/// layout plus both terminal identities; never fake a resized preview locally.
fn pane_resize_request(
    ui: &mut Ui,
    divider: model::PaneDivider,
    layout: model::Layout,
    cells: i32,
) -> Step {
    let cells = cells.clamp(divider.minimum, divider.maximum);
    if cells == 0 {
        ui.status = "Pane size unchanged".into();
        ui.status_error = false;
        return Step::Continue;
    }
    if ui.action_pending || ui.dialog.is_some() {
        ui.status = "Finish the current action before resizing panes".into();
        return Step::Continue;
    }
    if ui
        .tab_layout(&divider.leading)
        .is_none_or(|current| !current.same_geometry(&layout))
    {
        ui.status = "Pane layout changed; refresh and drag the divider again".into();
        ui.status_error = true;
        return Step::Continue;
    }
    ui.status = "Resizing terminal panes…".into();
    ui.status_error = false;
    Step::Send(
        json!({"op":"action","page":WORKSPACES,"action":"terminal.resize","node":divider.leading,
        "resize":{"peer":divider.trailing,"axis":if divider.horizontal {"horizontal"} else {"vertical"},"cells":cells,"layout":layout},"view":view_state(ui)}),
    )
}
fn focus_resize_key(ui: &mut Ui, key: KeyEvent) -> Option<Step> {
    if ui.page != WORKSPACES || ui.view != View::Focus || !key.modifiers.contains(KeyModifiers::ALT)
    {
        return None;
    }
    let (horizontal, delta, prefer_leading) = match key.code {
        KeyCode::Left => (true, -2, false),
        KeyCode::Right => (true, 2, true),
        KeyCode::Up => (false, -1, false),
        KeyCode::Down => (false, 1, true),
        _ => return None,
    };
    let current = ui
        .terminal_node(&ui.focus_target)
        .map(|node| node.id.clone())
        .unwrap_or_default();
    let divider = ui
        .pane_dividers
        .iter()
        .filter(|edge| {
            edge.horizontal == horizontal && (edge.leading == current || edge.trailing == current)
        })
        .min_by_key(|edge| {
            if prefer_leading {
                edge.leading != current
            } else {
                edge.trailing != current
            }
        })
        .cloned();
    let Some(divider) = divider.filter(|_| !ui.zoomed) else {
        ui.status = "Show a split tab to resize its panes".into();
        return Some(Step::Continue);
    };
    let Some(layout) = ui.tab_layout(&current).cloned() else {
        return Some(Step::Continue);
    };
    Some(pane_resize_request(ui, divider, layout, delta))
}
fn focus_resize_mouse(ui: &mut Ui, mouse: MouseEvent) -> Option<Step> {
    if ui.page != WORKSPACES || ui.view != View::Focus || ui.dialog.is_some() {
        return None;
    }
    let point = ratatui::layout::Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(event::MouseButton::Left) => {
            let divider = ui
                .pane_dividers
                .iter()
                .find(|edge| edge.area.contains(point))?
                .clone();
            if ui.action_pending || ui.zoomed {
                return Some(Step::Continue);
            }
            let layout = ui.tab_layout(&divider.leading)?.clone();
            ui.pane_resize = Some(model::PaneResizeDrag {
                divider,
                layout,
                start: (mouse.column, mouse.row),
                cells: 0,
            });
            ui.status = "Drag to resize real panes · release to apply · Esc cancels".into();
            ui.status_error = false;
        }
        MouseEventKind::Drag(event::MouseButton::Left) => {
            let drag = ui.pane_resize.as_mut()?;
            drag.cells = drag.divider.cells(drag.start, (mouse.column, mouse.row));
            ui.status = format!(
                "Resize {:+} {} · release to apply · Esc cancels",
                drag.cells,
                if drag.divider.horizontal {
                    "columns"
                } else {
                    "rows"
                }
            );
        }
        MouseEventKind::Up(event::MouseButton::Left) => {
            let mut drag = ui.pane_resize.take()?;
            drag.cells = drag.divider.cells(drag.start, (mouse.column, mouse.row));
            return Some(pane_resize_request(
                ui,
                drag.divider,
                drag.layout,
                drag.cells,
            ));
        }
        _ => return None,
    }
    Some(Step::Continue)
}

fn border_mouse(ui: &mut Ui, mouse: MouseEvent, p: ratatui::layout::Position) -> Option<Step> {
    let under = ui.handles.iter().find(|h| h.area.contains(p)).copied();
    match mouse.kind {
        MouseEventKind::Down(event::MouseButton::Left) => {
            let handle = under?;
            let again = ui.handle_clicked.is_some_and(|(s, at)| {
                s == handle.split && at.elapsed() < Duration::from_millis(500)
            });
            ui.handle_clicked = Some((handle.split, Instant::now()));
            if again {
                ui.dragging = None;
                ui.sizes.reset(handle.split);
                resized(ui, handle.split);
            } else {
                ui.dragging = Some(handle);
            }
        }
        MouseEventKind::Drag(event::MouseButton::Left) => {
            let handle = ui.dragging?;
            if ui
                .sizes
                .set(handle.split, handle.value(mouse.column, mouse.row))
            {
                resized(ui, handle.split);
            }
        }
        MouseEventKind::Up(event::MouseButton::Left) => {
            ui.dragging.take()?;
        }
        MouseEventKind::Moved => {
            ui.hovered = under.map(|h| h.split);
            return None;
        }
        _ => return None,
    }
    Some(Step::Continue)
}

/// Keys that move a border: `<` `>` the Details border, `+` `-` the live
/// screen (tile size in Tiles), `=` every size back to its default. Alt and
/// an arrow move the same borders the way the arrow points.
fn resize_key(ui: &mut Ui, key: KeyEvent) -> bool {
    if ui.page == WORKSPACES
        && (ui.view == View::Cube
            || (ui.view.grid() && matches!(key.code, KeyCode::Up | KeyCode::Down)))
        && key.modifiers.contains(KeyModifiers::ALT)
        && matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
        )
    {
        return false;
    }
    let cube = ui.page == WORKSPACES && ui.view == View::Cube;
    let tiles = ui.page == WORKSPACES && ui.view == View::Tiles;
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let (split, step) = match key.code {
        KeyCode::Char('<') if cube => (Split::CubeWidth, -5),
        KeyCode::Char('>') if cube => (Split::CubeWidth, 5),
        KeyCode::Char('+') if cube => (Split::CubeHeight, 5),
        KeyCode::Char('-' | '_') if cube => (Split::CubeHeight, -5),
        KeyCode::Char('<') => (Split::Details, 5),
        KeyCode::Char('>') => (Split::Details, -5),
        KeyCode::Left if alt => (Split::Details, 5),
        KeyCode::Right if alt => (Split::Details, -5),
        KeyCode::Char('+') if tiles => (Split::Tiles, 25),
        KeyCode::Char('-' | '_') if tiles => (Split::Tiles, -25),
        KeyCode::Char('+') => (Split::Screen, 5),
        KeyCode::Char('-' | '_') => (Split::Screen, -5),
        KeyCode::Down if alt => (Split::Screen, 5),
        KeyCode::Up if alt => (Split::Screen, -5),
        KeyCode::Char('\\') => {
            toggle_sidebar(ui);
            return true;
        }
        KeyCode::Char('b') if !alt => {
            cycle_look(ui, true);
            return true;
        }
        KeyCode::Char('s') if key.modifiers.is_empty() => {
            toggle_saver(ui);
            return true;
        }
        KeyCode::Char('=') => {
            change_option(ui, Item::Reset, true);
            return true;
        }
        _ => return false,
    };
    let value = i32::from(ui.sizes.get(split)) + step;
    ui.sizes.set(split, value);
    resized(ui, split);
    true
}

/// Turn the screensaver on or off; the choice is remembered with the view.
/// Latest activity first, or as the backends lay terminals out; the
/// selected terminal stays selected.
fn toggle_order(ui: &mut Ui) {
    let selected = ui.selected_terminal();
    ui.order = match ui.order {
        Order::Activity => Order::Layout,
        Order::Layout => Order::Activity,
    };
    if let Some(id) = selected {
        ui.reveal(&id);
    }
    ui.sizes_changed = Some(Instant::now());
    ui.last_preview = None;
    ui.status_error = false;
    ui.status = match ui.order {
        Order::Activity => "Latest activity first · o keeps the layout order".into(),
        Order::Layout => "Layout order · o puts the latest activity first".into(),
    };
}

fn toggle_saver(ui: &mut Ui) {
    ui.saver.enabled = !ui.saver.enabled;
    ui.sizes_changed = Some(Instant::now());
    ui.status_error = false;
    let after = ui.theme.screensaver;
    ui.status = if !ui.saver.enabled {
        "Screensaver off · s turns it on".into()
    } else if !ui.theme.backdrop {
        "Screensaver on, but it needs true color and Unicode symbols, \
         and DAEDALUS_BACKDROP not set to 0"
            .into()
    } else if after == 0 {
        "Screensaver on, but DAEDALUS_SCREENSAVER=0 keeps it from starting".into()
    } else {
        format!(
            "Screensaver on · starts after {} idle · s turns it off",
            idle(after)
        )
    };
}

/// Idle time in words: "45 s", "3 min", "1 min 30 s".
fn idle(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// The screensaver may start only over the plain interface: not during the
/// splash, an action you started, a question, an open terminal, a search or
/// a drag.
/// Whether `request` is an action. Only an action's answer may move the page
/// to where the backend left it; a refresh, like the automatic one, leaves
/// the page as it is.
fn is_action(request: &Value) -> bool {
    request["op"] == "action"
}

fn may_rest(ui: &Ui) -> bool {
    ui.theme.backdrop
        && ui.splash.is_none()
        && !ui.action_pending
        && ui.dialog.is_none()
        && ui.options.is_none()
        && ui.binding_search.is_none()
        && ui.embed.is_none()
        && !ui.searching
        && ui.dragging.is_none()
}

/// Deliberate keys, clicks, scrolls or paste restart the idle wait. True
/// when it woke the screensaver instead: that input does nothing else,
/// except s, which also turns the screensaver off.
fn saver_input(ui: &mut Ui, event: &Event) -> bool {
    let deliberate = match event {
        Event::Key(key) => key.kind != KeyEventKind::Release,
        Event::Mouse(mouse) => mouse.kind != MouseEventKind::Moved,
        Event::Paste(_) => true,
        _ => false,
    };
    if !deliberate || !ui.saver.touch(Instant::now()) {
        return false;
    }
    ui.paged_at = Some(Instant::now());
    if let Event::Key(key) = event
        && key.code == KeyCode::Char('s')
        && key.modifiers.is_empty()
    {
        toggle_saver(ui);
    }
    true
}

/// Leave the screensaver; the page fades back in.
fn wake(ui: &mut Ui) {
    if ui.saver.since.take().is_some() {
        ui.paged_at = Some(Instant::now());
    }
}

/// Follow the splash image request. Cache pixels once; camera motion only
/// changes the placement. The screensaver draws glitching block art instead.
fn show_picture(ui: &mut Ui, out: &mut impl Write) -> io::Result<()> {
    let wanted = ui.splash.as_ref().and_then(|splash| splash.hires);
    // Reassert the cached placement after each draw. Terminal clears can erase
    // placements without notifying us; pixels remain cached in the terminal.
    if wanted == ui.kitty_shown && (wanted.is_none() || ui.splash.is_some()) {
        return Ok(());
    }
    match wanted {
        Some(area) => graphics::place(out, area)?,
        None => graphics::remove(out, false)?,
    }
    ui.kitty_shown = wanted;
    Ok(())
}

/// Fold the sidebar to its icon rail, or open it at its previous width.
fn toggle_sidebar(ui: &mut Ui) {
    if ui.sizes.sidebar > 0 {
        ui.sidebar_open = ui.sizes.sidebar;
        ui.sizes.sidebar = 0;
    } else if ui.sidebar_open > 0 {
        ui.sizes.sidebar = ui.sidebar_open;
    } else {
        ui.sizes.reset(Split::Sidebar);
    }
    resized(ui, Split::Sidebar);
}

/// The next or previous background, where the terminal can show one.
fn cycle_look(ui: &mut Ui, forward: bool) {
    ui.status_error = false;
    if !ui.theme.backdrop {
        ui.status = "The background light and artwork need true color and \
                     Unicode symbols, and DAEDALUS_BACKDROP not set to 0"
            .into();
        return;
    }
    // Four looks: three steps forward are one back.
    for _ in 0..if forward { 1 } else { 3 } {
        ui.look = ui.look.next();
    }
    ui.status = format!("Background: {} · b changes it", ui.look.describe());
    ui.sizes_changed = Some(Instant::now());
}

fn open_options(ui: &mut Ui) {
    ui.options = Some(OptionsMenu {
        opened: Some(Instant::now()),
        ..OptionsMenu::default()
    });
    ui.status = "Options: changes apply at once and are remembered".into();
    ui.status_error = false;
}

/// Keys while Options is open: arrows choose and change, the item's own
/// key changes it too, Esc or , closes.
fn options_key(ui: &mut Ui, key: KeyEvent) {
    let Some(menu) = ui.options.as_mut() else {
        return;
    };
    let last = Item::ALL.len() - 1;
    let item = Item::ALL[menu.selected.min(last)];
    match key.code {
        KeyCode::Esc | KeyCode::Char(',' | 'q') => {
            ui.options = None;
            ui.status = "Options closed · , opens them again".into();
            ui.status_error = false;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            menu.selected = menu.selected.checked_sub(1).unwrap_or(last);
            menu.moved = Some(Instant::now());
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            menu.selected = if menu.selected >= last {
                0
            } else {
                menu.selected + 1
            };
            menu.moved = Some(Instant::now());
        }
        KeyCode::Home => menu.selected = 0,
        KeyCode::End => menu.selected = last,
        KeyCode::Left => change_option(ui, item, false),
        KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => change_option(ui, item, true),
        KeyCode::Char(c) => {
            if let Some(item) = Item::by_key(c) {
                menu.selected = Item::ALL.iter().position(|i| *i == item).unwrap_or(0);
                change_option(ui, item, true);
            }
        }
        _ => {}
    }
}

/// Change one preference, say what it is now and remember it.
fn change_option(ui: &mut Ui, item: Item, forward: bool) {
    if let Some(menu) = ui.options.as_mut() {
        menu.moved = Some(Instant::now());
    }
    ui.status_error = false;
    match item {
        Item::Order => toggle_order(ui),
        Item::Sidebar => toggle_sidebar(ui),
        Item::Look => cycle_look(ui, forward),
        Item::Saver => toggle_saver(ui),
        Item::Tiles => {
            let step = if forward { 25 } else { -25 };
            let value = i32::from(ui.sizes.get(Split::Tiles)) + step;
            ui.sizes.set(Split::Tiles, value);
            resized(ui, Split::Tiles);
        }
        Item::Settings => {
            ui.options = None;
            ui.navigate(4);
        }
        Item::Updates => {
            ui.options = None;
            ui.updates_requested = true;
        }
        Item::Reset => {
            ui.sizes = Sizes::default();
            ui.sizes_changed = Some(Instant::now());
            ui.status = "Panel sizes reset".into();
        }
        Item::Keys => {
            ui.hide_keys = !ui.hide_keys;
            ui.sizes_changed = Some(Instant::now());
            ui.status = if ui.hide_keys {
                "Key bar hidden · ? shows it".into()
            } else {
                "Key bar shown · ? hides it".into()
            };
        }
        Item::Motion if ui.still => {
            ui.status = "Reduced motion is set for this terminal (DAEDALUS_REDUCED_MOTION); \
                         unset it for neon motion"
                .into();
        }
        _ => {
            let Some(name) = item.switch() else {
                return;
            };
            let on = !ui.prefs.get(name).unwrap_or(true);
            ui.prefs.set(name, on);
            if item == Item::Motion {
                ui.theme.reduced_motion = !on;
            }
            ui.last_preview = None;
            ui.sizes_changed = Some(Instant::now());
            ui.status = format!(
                "{}: {} · , Options",
                item.label(),
                options::value(ui, item).to_lowercase()
            );
        }
    }
}

/// A terminal clicked in the sidebar: selected in Workspaces; clicked again
/// while selected, opened in Focus.
fn jump_to(ui: &mut Ui, id: &str) -> Step {
    let again = ui.page == WORKSPACES && ui.selected_terminal().as_deref() == Some(id);
    if ui.page != WORKSPACES {
        ui.navigate(WORKSPACES);
    }
    ui.reveal(id);
    if ui.selected_terminal().as_deref() != Some(id) {
        // Hidden by a search or the pinned-only filter: show everything.
        ui.filter.clear();
        ui.searching = false;
        ui.pinned_only = false;
        ui.reveal(id);
    }
    ui.last_preview = None;
    if again {
        return switch_view(ui, View::Focus);
    }
    Step::Continue
}

/// Say the new size and remember it once it settles.
fn resized(ui: &mut Ui, split: Split) {
    if matches!(split, Split::CubeWidth | Split::CubeHeight) {
        ui.inline_screen = None;
        if let Some(embed) = ui.embed.as_mut() {
            embed.origin = None;
        }
        ui.moved_at = None;
    }
    ui.status = split.describe(ui.sizes.get(split));
    ui.status_error = false;
    ui.sizes_changed = Some(Instant::now());
}

/// A key or a click skips the splash; it is not passed on.
fn skips_splash(event: &Event) -> bool {
    match event {
        Event::Key(key) => key.kind != KeyEventKind::Release,
        Event::Mouse(mouse) => matches!(mouse.kind, MouseEventKind::Down(_)),
        _ => false,
    }
}

/// Whether setup may open its question: not over the splash, which the
/// question would end at once, and not while an answer is awaited.
fn setup_due(ui: &Ui) -> bool {
    ui.splash.is_none() && !ui.busy && !ui.action_pending
}

/// Close the splash; the page fades in behind it.
fn end_splash(ui: &mut Ui) {
    if ui.splash.take().is_some() {
        ui.paged_at = Some(Instant::now());
    }
}

/// Redraw quickly only while something moves; idle frames stay cheap.
fn frame_interval(ui: &Ui) -> Duration {
    let recent = |at: Option<Instant>| at.is_some_and(|t| t.elapsed() < Duration::from_millis(400));
    if ui
        .embed
        .as_ref()
        .is_some_and(|e| !matches!(e.phase, embed::Phase::Ended(_)))
    {
        // A streaming terminal redraws as its output arrives.
        Duration::from_millis(16)
    } else if ui.splash.is_some() && ui.theme.animated() {
        Duration::from_millis(33)
    } else if ui.saver.since.is_some() {
        Duration::from_millis(if ui.theme.animated() { 50 } else { 1000 })
    } else if !ui.theme.animated() {
        Duration::from_millis(250)
    } else if ui.busy
        || recent(ui.moved_at)
        || recent(ui.paged_at)
        || ui.options.as_ref().is_some_and(|menu| recent(menu.moved))
    {
        Duration::from_millis(33)
    } else {
        Duration::from_millis(100)
    }
}

/// Right unfolds; Left folds, or moves to the parent of a folded/leaf node.
fn fold(ui: &mut Ui, open: bool) {
    let Some(row) = ui.selected_row().filter(|r| r.kind == Kind::Node) else {
        return;
    };
    if open {
        ui.collapsed.remove(&row.id);
    } else if row.children && !ui.collapsed.contains(&row.id) {
        ui.collapsed.insert(row.id);
    } else if !row.parent.is_empty() {
        ui.select_id(&row.parent);
    }
}

/// The backend request for the selected row, if it needs the backend at all.
fn activate(ui: &mut Ui) -> Option<Value> {
    let row = ui.selected_row()?;
    let request = match row.kind {
        Kind::Header => return None,
        Kind::Node => {
            let actionable = ui
                .snapshot
                .tree
                .get(row.source)
                .is_some_and(|n| n.actionable);
            // Groups fold locally; selecting one can never act on its children.
            if row.children && !actionable {
                if !ui.collapsed.remove(&row.id) {
                    ui.collapsed.insert(row.id);
                }
                return None;
            }
            terminal_request(ui, &row.id, &row.id)
        }
        Kind::Action | Kind::Entry => {
            json!({"op":"action","page":ui.page,"action":row.id,"view":view_state(ui)})
        }
    };
    ui.status = format!("Opening {}…", row.title);
    ui.status_error = false;
    Some(request)
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use std::process::Command;
    fn press(ui: &mut Ui, code: KeyCode) -> Option<Value> {
        match key_event(ui, KeyEvent::new(code, KeyModifiers::NONE)) {
            Step::Send(value) => Some(value),
            _ => None,
        }
    }
    fn workspace() -> Ui {
        model::tests::tiles_fixture()
    }
    #[test]
    fn quick_switcher_opens_exact_result_and_empty_search_never_falls_back() {
        let mut ui = workspace();
        key_event(
            &mut ui,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        );
        assert!(ui.switching && ui.searching);
        assert_eq!(ui.view, View::Tree);
        let node = ui.selected_terminal().unwrap();
        let step = choose_switcher(&mut ui, None);
        assert!(
            matches!(step,Step::Send(ref v) if v["action"]=="terminal.watch" && v["node"]==node)
        );
        assert!(!ui.switching && !ui.searching);
        open_switcher(&mut ui);
        ui.filter = "no_such_window_123".into();
        assert!(matches!(choose_switcher(&mut ui, Some(0)), Step::Continue));
        assert!(ui.switching);
        key_event(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.switching);
    }
    #[test]
    fn switcher_shortcut_uses_the_visible_filtered_result() {
        let mut ui = workspace();
        open_switcher(&mut ui);
        let expected = ui
            .rows()
            .into_iter()
            .find(|r| r.kind == Kind::Node)
            .unwrap()
            .id;
        let step = key_event(
            &mut ui,
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT),
        );
        assert!(matches!(step,Step::Send(ref v) if v["node"]==expected));
        assert!(!ui.switching);
    }
    #[test]
    fn refreshing_keeps_the_page() {
        // r refreshes like the automatic refresh: it is no action, so its
        // answer never moves the page to where the backend last acted.
        let mut ui = workspace();
        ui.navigate(OVERVIEW);
        let request = press(&mut ui, KeyCode::Char('r')).expect("r refreshes");
        assert_eq!(request["op"], "refresh");
        assert!(!is_action(&request));
        assert_eq!(ui.page, OVERVIEW);
        assert!(is_action(
            &json!({"op":"action","page":OVERVIEW,"action":"setup"})
        ));
    }
    #[test]
    fn resize_keys_move_borders_within_limits() {
        let mut ui = workspace();
        ui.view = View::Tree;
        let before = ui.sizes;
        assert!(press(&mut ui, KeyCode::Char('<')).is_none());
        assert_eq!(ui.sizes.details, before.details + 5);
        assert!(ui.sizes_changed.is_some());
        assert!(ui.status.contains("Details"), "{}", ui.status);
        press(&mut ui, KeyCode::Char('+'));
        assert_eq!(ui.sizes.screen, before.screen + 5);
        for _ in 0..40 {
            press(&mut ui, KeyCode::Char('>'));
        }
        assert_eq!(ui.sizes.details, Split::Details.limits().0);
        // Alt and an arrow move the same borders the way the arrow points.
        key_event(&mut ui, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(ui.sizes.details, Split::Details.limits().0 + 5);
        key_event(&mut ui, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(ui.sizes.screen, before.screen);
        // In Tiles, + and - size the tiles instead.
        ui.view = View::Tiles;
        press(&mut ui, KeyCode::Char('+'));
        assert_eq!(ui.sizes.tiles, 125);
        press(&mut ui, KeyCode::Char('='));
        assert_eq!(ui.sizes, Sizes::default());
        assert_eq!(
            view_state(&ui)["sizes"]["details"],
            Sizes::default().details
        );
        // Typing in search never resizes.
        ui.searching = true;
        press(&mut ui, KeyCode::Char('<'));
        assert_eq!(ui.sizes, Sizes::default());
        assert_eq!(ui.filter, "<");
    }
    #[test]
    fn the_sidebar_folds_to_a_rail_and_opens_at_its_width() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = workspace();
        ui.view = View::Tree;
        ui.sizes.sidebar = 30;
        press(&mut ui, KeyCode::Char('\\'));
        assert_eq!(ui.sizes.sidebar, 0);
        assert!(ui.status.contains("folded"), "{}", ui.status);
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        // Every section is still one click away on the rail.
        assert_eq!(ui.nav_hits.len(), ui.page_count());
        assert!(ui.nav_hits.iter().all(|(r, _)| r.right() <= Sizes::RAIL));
        let (rect, page) = ui.nav_hits[5];
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        mouse_event(&mut ui, click(rect.x + 1, rect.y));
        assert_eq!(ui.page, page);
        // » opens it again at the width it had.
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (button, _) = ui
            .quick_hits
            .iter()
            .find(|(_, a)| a == "sidebar")
            .unwrap()
            .clone();
        mouse_event(&mut ui, click(button.x + 2, button.y));
        assert_eq!(ui.sizes.sidebar, 30);
        // « on the open sidebar folds it.
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (button, _) = ui
            .quick_hits
            .iter()
            .find(|(_, a)| a == "sidebar")
            .unwrap()
            .clone();
        assert!(button.x > Sizes::RAIL);
        mouse_event(&mut ui, click(button.x + 1, button.y));
        assert_eq!(ui.sizes.sidebar, 0);
        // Dragging the rail's edge out opens it where the pointer is.
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let edge = *ui
            .handles
            .iter()
            .find(|h| h.split == Split::Sidebar)
            .unwrap();
        let at = |kind, column| MouseEvent {
            kind,
            column,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        let left = event::MouseButton::Left;
        mouse_event(&mut ui, at(MouseEventKind::Down(left), edge.area.x));
        mouse_event(&mut ui, at(MouseEventKind::Drag(left), 27));
        mouse_event(&mut ui, at(MouseEventKind::Up(left), 27));
        assert_eq!(ui.sizes.sidebar, 28);
    }
    #[test]
    fn b_cycles_the_background_and_it_is_remembered_with_the_view() {
        let mut ui = workspace();
        ui.theme.backdrop = true;
        assert_eq!(view_state(&ui)["backdrop"], "glow");
        press(&mut ui, KeyCode::Char('b'));
        assert_eq!(ui.look, backdrop::Look::Art);
        assert_eq!(ui.status, "Background: artwork, faint · b changes it");
        assert!(ui.sizes_changed.is_some());
        press(&mut ui, KeyCode::Char('b'));
        press(&mut ui, KeyCode::Char('b'));
        assert_eq!(view_state(&ui)["backdrop"], "off");
        // Without true color there is nothing to change; the reason is shown.
        ui.theme.backdrop = false;
        press(&mut ui, KeyCode::Char('b'));
        assert_eq!(ui.look, backdrop::Look::Off);
        assert!(ui.status.contains("true color"), "{}", ui.status);
    }
    #[test]
    fn the_splash_waits_for_the_inventory_and_a_key_skips_it() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = workspace();
        ui.theme.backdrop = true;
        let mut splash = Splash::new();
        // Shown long enough, but the inventory is not ready: it stays.
        splash.started = Instant::now() - Duration::from_secs_f32(Splash::MIN + 0.1);
        assert!(splash.advance(true));
        assert!(splash.leaving.is_none());
        splash.ready = Some(Instant::now());
        assert!(splash.advance(true), "it fades out first");
        assert!(splash.leaving.is_some());
        splash.leaving = Some(Instant::now() - Duration::from_secs_f32(Splash::LEAVE));
        assert!(!splash.advance(true));
        // The full-resolution picture keeps it up a little longer.
        let mut sharp = Splash::new();
        sharp.started = Instant::now() - Duration::from_secs_f32(Splash::MIN + 0.1);
        sharp.ready = Some(Instant::now());
        sharp.hires = Some(ratatui::layout::Rect::new(0, 0, 10, 5));
        assert!(sharp.advance(true) && sharp.leaving.is_none());
        sharp.started -= Duration::from_secs_f32(Splash::SHARP);
        assert!(sharp.advance(true) && sharp.leaving.is_some());
        // Ready at once: it still shows its minimum, unless motion is reduced.
        let mut quick = Splash::new();
        quick.ready = Some(Instant::now());
        assert!(quick.advance(true) && quick.leaving.is_none());
        // It never outstays its maximum, ready or not.
        let mut slow = Splash::new();
        slow.started = Instant::now() - Duration::from_secs_f32(Splash::MAX);
        slow.advance(true);
        assert!(slow.leaving.is_some());
        // While it shows, it covers the whole interface.
        ui.splash = Some(Splash::new());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Discovering terminals and agents"));
        assert!(!text.contains("Tiles"));
        // A key or a click skips it and is not passed on.
        let key = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(skips_splash(&key));
        assert!(!skips_splash(&Event::Resize(80, 24)));
        end_splash(&mut ui);
        assert!(ui.splash.is_none() && ui.paged_at.is_some());
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Tiles"));
    }
    #[test]
    fn the_screensaver_waits_for_idle_time_and_any_input_wakes_it() {
        let mut ui = workspace();
        ui.theme.backdrop = true;
        ui.theme.screensaver = 30;
        let now = Instant::now();
        ui.saver.last_input = now - Duration::from_secs(29);
        assert!(!ui.saver.due(30, now));
        ui.saver.last_input = now - Duration::from_secs(31);
        assert!(ui.saver.due(30, now) && may_rest(&ui));
        assert!(!ui.saver.due(0, now), "0 never starts it");
        // Never over a running action, a search or the splash.
        ui.searching = true;
        assert!(!may_rest(&ui));
        ui.searching = false;
        ui.splash = Some(Splash::new());
        assert!(!may_rest(&ui));
        ui.splash = None;
        ui.action_pending = true;
        assert!(!may_rest(&ui));
        ui.action_pending = false;
        ui.saver.since = Some(now - Duration::from_secs(2));
        assert!(!ui.saver.due(30, now), "already showing");
        assert!(!previews_due(&ui));
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains(" s  Disable screensaver  any key  Return "));
        assert!(!screen.contains("Workspaces"), "the interface is hidden");
        // Passive motion must not dismiss the artwork. A click wakes it.
        let moved = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        });
        let page = ui.page;
        assert!(!saver_input(&mut ui, &moved));
        assert!(ui.saver.since.is_some());
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(!saver_input(&mut ui, &release));
        assert!(ui.saver.since.is_some());
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        });
        assert!(saver_input(&mut ui, &click));
        assert!(ui.saver.since.is_none() && ui.paged_at.is_some() && ui.page == page);
        assert!(ui.saver.enabled);
        // Once awake, input goes through again and keeps the wait fresh.
        assert!(!saver_input(&mut ui, &moved));
        assert!(!ui.saver.due(30, Instant::now()));
        // s on the screensaver wakes it and turns it off.
        ui.saver.since = Some(Instant::now());
        let s = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(saver_input(&mut ui, &s));
        assert!(!ui.saver.enabled && ui.saver.since.is_none());
        // A dialog wakes it too.
        ui.saver.since = Some(Instant::now());
        wake(&mut ui);
        assert!(ui.saver.since.is_none());
    }

    #[test]
    fn s_turns_the_screensaver_on_and_off_and_it_is_remembered() {
        let mut ui = workspace();
        ui.theme.backdrop = true;
        ui.theme.screensaver = 5;
        assert_eq!(view_state(&ui)["saver"], "on");
        // Its switch is always in sight on the status row.
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut status_row = |ui: &mut Ui| {
            terminal.draw(|f| view::draw(f, ui)).unwrap();
            let buffer = terminal.backend().buffer();
            (0..buffer.area.width)
                .map(|x| buffer[(x, buffer.area.height - 2)].symbol().to_string())
                .collect::<String>()
        };
        assert!(status_row(&mut ui).contains(" s  Disable screensaver"));
        press(&mut ui, KeyCode::Char('s'));
        assert!(!ui.saver.enabled && ui.sizes_changed.is_some());
        assert_eq!(view_state(&ui)["saver"], "off");
        assert_eq!(ui.status, "Screensaver off · s turns it on");
        assert!(status_row(&mut ui).contains(" s  Enable screensaver"));
        press(&mut ui, KeyCode::Char('s'));
        assert_eq!(
            ui.status,
            "Screensaver on · starts after 5 s idle · s turns it off"
        );
        ui.theme.screensaver = 180;
        ui.saver.enabled = false;
        press(&mut ui, KeyCode::Char('s'));
        assert!(ui.status.contains("after 3 min idle"));
        ui.theme.screensaver = 90;
        ui.saver.enabled = false;
        press(&mut ui, KeyCode::Char('s'));
        assert!(ui.status.contains("after 1 min 30 s"));
        ui.saver.enabled = false;
        ui.theme.backdrop = false;
        press(&mut ui, KeyCode::Char('s'));
        assert!(ui.status.contains("needs true color"));
        // While searching, s is typed, and the switch is not offered.
        ui.searching = true;
        assert!(!status_row(&mut ui).contains("screensaver"));
        press(&mut ui, KeyCode::Char('s'));
        assert!(ui.saver.enabled && ui.filter.ends_with('s'));
    }

    #[test]
    fn the_screensaver_is_glitch_art_even_where_kitty_graphics_work() {
        let mut ui = workspace();
        ui.theme.backdrop = true;
        ui.kitty = Some(graphics::Kitty { cell: (8.0, 24.0) });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        // Moments well inside a burst and well inside a calm stretch, after
        // the picture has faded in.
        let steady = |t: f32, on: bool| {
            (0..=10).all(|k| backdrop::glitching(t - 0.05 + k as f32 * 0.01) == on)
        };
        let moments: Vec<f32> = (130..3000).map(|n| n as f32 * 0.01).collect();
        let wild = moments.iter().copied().find(|&t| steady(t, true)).unwrap();
        let calm = moments.iter().copied().find(|&t| steady(t, false)).unwrap();
        let blocks = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .filter(|cell| {
                    cell.symbol()
                        .chars()
                        .any(|c| ('\u{2580}'..='\u{259f}').contains(&c))
                })
                .count()
        };
        // A picture left by the splash is taken down once the screensaver runs.
        ui.kitty_shown = Some(ratatui::layout::Rect::new(21, 0, 117, 39));
        let mut out = Vec::new();
        let mut frames = Vec::new();
        for t in [calm, wild] {
            ui.saver.since = Some(Instant::now() - Duration::from_secs_f32(t));
            out.clear();
            terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
            show_picture(&mut ui, &mut out).unwrap();
            let written = String::from_utf8_lossy(&out);
            assert!(ui.kitty_shown.is_none(), "at {t} s");
            assert!(!written.contains("a=p") && !written.contains("a=t"));
            // The block picture with scanlines and color split fills the screen.
            assert!(blocks(&terminal) > 1000, "at {t} s");
            frames.push(terminal.backend().buffer().clone());
        }
        assert!(String::from_utf8_lossy(&out).is_empty() || ui.kitty_shown.is_none());
        assert_ne!(frames[0], frames[1], "A glitch burst changes the picture");
        // Reduced motion shows the still picture, still without the overlay.
        ui.theme.reduced_motion = true;
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        assert!(ui.kitty_shown.is_none());
        assert!(blocks(&terminal) > 1000);
    }
    #[test]
    fn the_full_resolution_picture_follows_the_splash() {
        let mut ui = workspace();
        ui.theme.backdrop = true;
        ui.theme.splash = true;
        ui.kitty = Some(graphics::Kitty { cell: (10.0, 20.0) });
        let mut splash = Splash::new();
        splash.started = Instant::now() - Duration::from_secs_f32(0.2);
        ui.splash = Some(splash);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut out = Vec::new();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        assert!(out.is_empty(), "not while the light spreads");
        ui.splash.as_mut().unwrap().started = Instant::now() - Duration::from_secs_f32(1.2);
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        let shown = ui.kitty_shown.expect("placed");
        // Square in pixels over the block picture: 68 columns of 10 by 34 rows of 20.
        assert_eq!(shown, ratatui::layout::Rect::new(26, 1, 68, 34));
        assert!(String::from_utf8_lossy(&out).contains("a=p,i=7183"));
        out.clear();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        assert!(out.is_empty(), "placed once");
        end_splash(&mut ui);
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        assert!(String::from_utf8_lossy(&out).contains("a=d,d=i,i=7183"));
        assert!(ui.kitty_shown.is_none());
        // Without kitty graphics nothing is ever written.
        ui.kitty = None;
        ui.splash = Some(Splash::new());
        ui.splash.as_mut().unwrap().started = Instant::now() - Duration::from_secs(2);
        out.clear();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        show_picture(&mut ui, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn borders_drag_like_pane_borders_and_a_double_click_resets() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = workspace();
        ui.view = View::Tree;
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let border = |ui: &Ui| {
            *ui.handles
                .iter()
                .find(|h| h.split == Split::Details)
                .expect("Details border")
        };
        let at = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let left = event::MouseButton::Left;
        let (x, y) = (border(&ui).area.x, border(&ui).area.y + 5);
        mouse_event(&mut ui, at(MouseEventKind::Moved, x, y));
        assert_eq!(ui.hovered, Some(Split::Details));
        mouse_event(&mut ui, at(MouseEventKind::Down(left), x, y));
        assert!(ui.dragging.is_some());
        mouse_event(&mut ui, at(MouseEventKind::Drag(left), x - 20, y));
        assert!(ui.sizes.details > Sizes::default().details);
        // The panel's left border follows the pointer.
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let edge = i32::from(border(&ui).area.x) + 1;
        assert!(
            (edge - i32::from(x - 20)).abs() <= 1,
            "{edge} vs {}",
            x - 20
        );
        mouse_event(&mut ui, at(MouseEventKind::Up(left), x - 20, y));
        assert!(ui.dragging.is_none());
        assert!(ui.sizes_changed.is_some());
        // Two quick clicks on the border put it back.
        let x = border(&ui).area.x;
        mouse_event(&mut ui, at(MouseEventKind::Down(left), x, y));
        mouse_event(&mut ui, at(MouseEventKind::Up(left), x, y));
        mouse_event(&mut ui, at(MouseEventKind::Down(left), x, y));
        assert_eq!(ui.sizes.details, Sizes::default().details);
        assert!(ui.dragging.is_none());
    }
    #[test]
    fn cube_resize_keys_and_drag_keep_selection_and_layout() {
        let mut ui = workspace();
        ui.view = View::Cube;
        ui.reveal("a");
        let selected = ui.selected_terminal();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        key_event(&mut ui, key('>'));
        key_event(&mut ui, key('+'));
        assert_eq!((ui.sizes.cube_width, ui.sizes.cube_height), (65, 55));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 50)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let handle = *ui
            .handles
            .iter()
            .find(|h| h.split == Split::CubeWidth)
            .unwrap();
        let event = |kind, x| MouseEvent {
            kind,
            column: x,
            row: handle.area.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        let left = event::MouseButton::Left;
        mouse_event(&mut ui, event(MouseEventKind::Down(left), handle.area.x));
        mouse_event(
            &mut ui,
            event(MouseEventKind::Drag(left), handle.area.x + 8),
        );
        mouse_event(&mut ui, event(MouseEventKind::Up(left), handle.area.x + 8));
        assert!(ui.sizes.cube_width > 65);
        assert_eq!(ui.view, View::Cube);
        assert_eq!(ui.selected_terminal(), selected);
        assert!(ui.sizes_changed.is_some());
        key_event(&mut ui, key('='));
        assert_eq!((ui.sizes.cube_width, ui.sizes.cube_height), (60, 50));
    }

    #[test]
    fn cube_single_click_attaches_and_small_faces_reject_input() {
        let mut ui = workspace();
        ui.set_view(View::Cube);
        ui.reveal("a");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (screen, _) = ui.pane_hits.iter().find(|(_, id)| id == "a").unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: screen.x + 1,
            row: screen.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        let Step::Send(request) = mouse_event(&mut ui, click) else {
            panic!("A single click attaches a readable face");
        };
        assert_eq!(request["action"], "terminal.attach");
        assert_eq!(request["node"], "a");

        let mut tiny = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
        tiny.draw(|f| view::draw(f, &mut ui)).unwrap();
        assert!(ui.pane_hits.is_empty());
        assert!(matches!(
            quick_action(&mut ui, "terminal.attach"),
            Step::Continue
        ));
        assert!(ui.status.contains("too small"));
        assert!(ui.embed.is_none());
        assert_eq!(ui.view, View::Cube);
    }

    #[test]
    fn cube_clicking_empty_space_detaches_without_changing_layout() {
        let mut ui = workspace();
        ui.set_view(View::Cube);
        ui.reveal("a");
        let plan = json!({
            "adapter":"herdr-session", "mode":"control", "node":"a",
            "argv":[crate::test_support::path("/bin/sh"), "-c", "while read line; do :; done"]
        });
        open_embed(&mut ui, &plan);
        assert!(ui.embed.is_some());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let face = ui.tile_hits[0].0;
        let blank = (0..40)
            .flat_map(|row| (0..140).map(move |column| (column, row)))
            .find(|&(column, row)| {
                let point = ratatui::layout::Position::new(column, row);
                !face.contains(point)
                    && !ui.tile_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.nav_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.side_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.view_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.quick_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.list_hits.iter().any(|(rect, _)| rect.contains(point))
                    && !ui.handles.iter().any(|handle| handle.area.contains(point))
            })
            .unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: blank.0,
            row: blank.1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(mouse_event(&mut ui, click), Step::Continue));
        assert!(ui.embed.is_none());
        assert_eq!(ui.view, View::Cube);
        assert!(ui.status.contains("Detached"));
    }

    #[test]
    fn cube_alt_arrows_switch_workspaces_and_front_terminals_independently() {
        let mut ui = workspace();
        ui.snapshot.workspace.pins.clear();
        ui.snapshot.tree.push(model::Node {
            id: "ws-a".into(),
            kind: "workspace".into(),
            label: "Alpha".into(),
            ..model::Node::default()
        });
        ui.snapshot.tree.push(model::Node {
            id: "ws-b".into(),
            kind: "workspace".into(),
            label: "Beta".into(),
            ..model::Node::default()
        });
        for node in &mut ui.snapshot.tree {
            if node.tile.is_some() {
                node.parent = "ws-a".into();
            }
        }
        let mut node = ui
            .snapshot
            .tree
            .iter()
            .find(|n| n.tile.is_some())
            .unwrap()
            .clone();
        node.id = "other-workspace-terminal".into();
        node.parent = "ws-b".into();
        ui.snapshot.tree.push(node);
        ui.set_view(View::Cube);
        let groups = view::cube_groups(&ui, &ui.tiles());
        ui.select_tile(groups[0][0]);
        let original = ui.selected_terminal().unwrap();
        let alt = |code| KeyEvent::new(code, KeyModifiers::ALT);
        assert!(matches!(
            key_event(&mut ui, alt(KeyCode::Right)),
            Step::Continue
        ));
        assert_eq!(ui.cube_anchor.as_deref(), Some(original.as_str()));
        assert!(groups[0].contains(&ui.tile));
        let remembered_front = ui.selected_terminal().unwrap();
        assert!(matches!(
            key_event(&mut ui, alt(KeyCode::Down)),
            Step::Continue
        ));
        assert_eq!(ui.tile, groups[1][0]);
        assert!(ui.cube_anchor.is_none());
        cube_key(&mut ui, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            ui.tile, groups[1][0],
            "rotation stays inside the single-terminal workspace"
        );
        key_event(&mut ui, alt(KeyCode::Down));
        assert!(groups[0].contains(&ui.tile), "workspace switching wraps");
        assert_eq!(ui.selected_terminal().unwrap(), remembered_front);
        assert_eq!(ui.cube_anchor.as_deref(), Some(original.as_str()));
        // A removed terminal cannot be restored from remembered state.
        key_event(&mut ui, alt(KeyCode::Down));
        ui.snapshot.tree.retain(|n| n.id != remembered_front);
        ui.reveal("other-workspace-terminal");
        key_event(&mut ui, alt(KeyCode::Up));
        assert_ne!(ui.selected_terminal().unwrap(), remembered_front);
        assert!(ui.terminal_node(&ui.selected_terminal().unwrap()).is_some());
    }
    #[test]
    fn cube_rotates_wraps_and_focus_returns_to_the_selected_face() {
        let mut ui = workspace();
        ui.set_view(View::Cube);
        ui.select_tile(0);
        let count = ui.tiles().len();
        assert!(count >= 2);
        assert!(press(&mut ui, KeyCode::Left).is_none());
        assert_eq!(ui.tile, count - 1);
        assert!(press(&mut ui, KeyCode::Right).is_none());
        assert_eq!(ui.tile, 0);
        assert!(press(&mut ui, KeyCode::Down).is_none());
        assert_eq!(ui.tile, 2 % count);
        assert!(press(&mut ui, KeyCode::Up).is_none());
        assert_eq!(ui.tile, 0);
        let id = ui.selected_terminal().unwrap();
        let request = press(&mut ui, KeyCode::Char('z')).unwrap();
        assert_eq!(request["node"], id);
        assert_eq!(ui.view, View::Focus);
        assert!(press(&mut ui, KeyCode::Esc).is_none());
        assert_eq!(ui.view, View::Cube);
        assert_eq!(ui.selected_terminal().unwrap(), id);
        let mode: daedalus_tui::core::layouts::ViewMode =
            serde_json::from_value(view_state(&ui)["mode"].clone()).unwrap();
        assert_eq!(mode, daedalus_tui::core::layouts::ViewMode::Cube);
    }
    #[test]
    fn views_cycle_and_focus_starts_read_only_on_the_exact_terminal() {
        let mut ui = workspace();
        assert!(ui.select_id("a"));
        assert!(press(&mut ui, KeyCode::Char(']')).is_none());
        assert_eq!(ui.view, View::Tiles);
        assert_eq!(ui.tiles()[ui.tile].id, "a");
        // Enter opens the terminal's menu, as in Tree.
        let menu = press(&mut ui, KeyCode::Enter).unwrap();
        assert_eq!(
            (menu["action"].as_str(), menu["node"].as_str()),
            (Some("a"), Some("a"))
        );
        assert_eq!(ui.view, View::Tiles);
        let request = press(&mut ui, KeyCode::Char('z')).unwrap();
        assert_eq!(ui.view, View::Focus);
        assert_eq!(request["action"], "terminal.watch");
        assert_eq!(request["node"], "a");
        assert_eq!(request["view"]["mode"], "focus");
        // Esc leaves Focus for the view it came from, on the same terminal.
        assert!(press(&mut ui, KeyCode::Esc).is_none());
        assert_eq!(ui.view, View::Tiles);
        assert_eq!(ui.tiles()[ui.tile].id, "a");
        assert!(press(&mut ui, KeyCode::Char('[')).is_none());
        assert_eq!(ui.view, View::Tree);
        assert_eq!(ui.selected_row().unwrap().id, "a");
    }
    #[test]
    fn o_switches_the_order_keeps_the_selection_and_is_remembered() {
        let mut ui = workspace();
        ui.view = View::Wall;
        ui.snapshot.tree[1].active_at = 50.0;
        ui.snapshot.tree[2].active_at = 10.0;
        ui.snapshot.workspace.pins.clear();
        assert_eq!(ui.tiles()[0].id, "a");
        ui.select_tile(1);
        assert!(press(&mut ui, KeyCode::Char('o')).is_none());
        assert_eq!(ui.order, Order::Layout);
        assert_eq!(
            ui.tiles()[ui.tile].id,
            "b",
            "the same terminal stays selected"
        );
        assert!(ui.status.contains("Layout order"), "{}", ui.status);
        assert!(ui.sizes_changed.is_some());
        assert_eq!(view_state(&ui)["order"], "layout");
        assert_eq!(view_state(&ui)["mode"], "wall");
        // The Wall sits between Tiles and Focus.
        assert!(press(&mut ui, KeyCode::Char('[')).is_none());
        assert_eq!(ui.view, View::Tiles);
        assert!(press(&mut ui, KeyCode::Char(']')).is_none());
        assert_eq!(ui.view, View::Wall);
    }
    #[test]
    fn arrows_clicks_and_z_move_around_a_split_tab_in_focus() {
        let mut ui = workspace();
        model::tests::split(&mut ui);
        ui.view = View::Focus;
        ui.previous = View::Tiles;
        ui.focus_target = "a".into();
        let request = press(&mut ui, KeyCode::Right).unwrap();
        assert_eq!(
            (request["action"].as_str(), request["node"].as_str()),
            (Some("terminal.watch"), Some("b")),
            "the neighbor opens read-only"
        );
        assert_eq!(ui.focus_target, "b");
        assert!(press(&mut ui, KeyCode::Right).is_none());
        assert!(ui.status.contains("No terminal that way"), "{}", ui.status);
        // Z shows one terminal alone, then the whole tab again.
        press(&mut ui, KeyCode::Char('Z'));
        assert!(ui.zoomed);
        press(&mut ui, KeyCode::Char('Z'));
        assert!(!ui.zoomed);
        // A click on another terminal of the tab moves Focus there.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (rect, id) = ui
            .pane_hits
            .iter()
            .find(|(_, id)| id == "a")
            .cloned()
            .unwrap();
        let left = event::MouseButton::Left;
        let step = mouse_event(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(left),
                column: rect.x + 2,
                row: rect.y + 2,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(step, Step::Send(_)));
        assert_eq!(ui.focus_target, id);
        // A terminal alone in its tab has nowhere to go.
        ui.snapshot.tree[0].layout = None;
        assert!(press(&mut ui, KeyCode::Left).is_none());
        assert!(ui.status.contains("alone"), "{}", ui.status);
    }
    #[test]
    fn quick_keys_target_one_terminal_and_never_a_missing_pin() {
        let mut ui = workspace();
        ui.view = View::Tiles;
        ui.select_tile(0);
        let request = press(&mut ui, KeyCode::Char('a')).unwrap();
        assert_eq!(
            (request["action"].as_str(), request["node"].as_str()),
            (Some("terminal.attach"), Some("b"))
        );
        ui.select_tile(1);
        assert!(press(&mut ui, KeyCode::Char('a')).is_none());
        assert!(ui.status_error);
        let forget = press(&mut ui, KeyCode::Char('u')).unwrap();
        assert_eq!(forget["action"], "unpin:k");
        assert!(forget.get("node").is_none());
        ui.select_tile(2);
        assert!(
            press(&mut ui, KeyCode::Char('u')).is_none(),
            "u only forgets missing pins"
        );
        let more = press(&mut ui, KeyCode::Char('m')).unwrap();
        assert_eq!(
            (more["action"].as_str(), more["node"].as_str()),
            (Some("a"), Some("a"))
        );
        // Group rows in Tree have no quick actions.
        ui.view = View::Tree;
        assert!(ui.select_id("server"));
        assert!(press(&mut ui, KeyCode::Char('p')).is_none());
    }
    #[test]
    fn leaving_workspaces_closes_focus() {
        let mut ui = workspace();
        ui.view = View::Tiles;
        ui.select_tile(2);
        press(&mut ui, KeyCode::Char('z'));
        assert_eq!(ui.view, View::Focus);
        press(&mut ui, KeyCode::Tab);
        assert_ne!(ui.page, WORKSPACES);
        assert_eq!(ui.view, View::Tiles);
        assert!(ui.embed.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn attach_response_releases_action_and_loads_neighbor_without_state_reply() {
        let mut ui = workspace();
        ui.busy = true;
        ui.action_pending = true;
        let plan = json!({
            "adapter":"herdr-session", "mode":"observe", "node":"a",
            "argv":[crate::test_support::path("/bin/sh"), "-c", "printf '%s\n' '{\"type\":\"terminal.frame\",\"width\":80,\"height\":24,\"full\":true,\"bytes\":\"bmVpZ2hib3I=\"}'; read line"]
        });
        open_embed(&mut ui, &plan);
        assert!(
            !ui.busy && !ui.action_pending,
            "Attach is the completed backend response; no state frame follows"
        );
        assert_eq!(ui.view, View::Focus);
        ui.preview_targets = vec!["b".into()];
        assert!(
            previews_due(&ui),
            "Neighbor previews must resume while attached"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let embed = ui.embed.as_mut().unwrap();
        while !embed.live() && Instant::now() < deadline {
            embed.pump();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(embed.live());
        assert!(embed.parser.screen().contents().contains("neighbor"));
        let step = focus_pane(&mut ui, "b");
        assert!(
            matches!(step, Step::Send(_)),
            "Another pane remains selectable"
        );
        assert!(!ui.busy && !ui.action_pending);
    }

    #[cfg(unix)]
    #[test]
    fn live_input_keeps_each_layout_and_detaches_before_selection_changes() {
        for layout in [View::Cube, View::Tiles, View::Wall, View::Focus] {
            let mut ui = workspace();
            ui.snapshot.workspace.pins.clear();
            ui.view = layout;
            ui.reveal("a");
            let plan = json!({
                "adapter":"herdr-session", "mode":"control", "node":"a",
                "argv":[crate::test_support::path("/bin/sh"), "-c", "printf '%s\n' '{\"type\":\"terminal.frame\",\"width\":80,\"height\":24,\"full\":true,\"bytes\":\"SU5MSU5F\"}'; while read line; do :; done"]
            });
            open_embed(&mut ui, &plan);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ui.embed.as_ref().unwrap().live() && Instant::now() < deadline {
                ui.embed.as_mut().unwrap().pump();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(ui.embed.as_ref().unwrap().live());
            assert_eq!(ui.view, layout);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 50)).unwrap();
            terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
            let screen = ui
                .inline_screen
                .expect("a visible input screen in every layout");
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("INLINE"), "{layout:?}");
            assert!(
                ui.quick_hits
                    .iter()
                    .any(|(_, action)| action == "telegram.route")
            );
            key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
            );
            let Step::Send(route) = key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            ) else {
                panic!("Telegram shortcut must open the selected pane's chat picker")
            };
            assert_eq!(route["action"], "telegram.route");
            assert_eq!(route["node"], "a");
            assert_eq!(ui.view, layout);
            assert!(ui.embed.is_some());
            if layout.grid() {
                assert!(
                    ui.tile_hits
                        .iter()
                        .any(|(rect, index)| *index == ui.tile
                            && rect.contains(screen.as_position()))
                        || layout == View::Wall
                );
                assert!(!text.contains("Whole tab"));
                assert!(!ui.preview_targets.iter().any(|id| id == "a"));
            }
            if layout == View::Cube {
                key_event(
                    &mut ui,
                    KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
                );
                key_event(
                    &mut ui,
                    KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE),
                );
                assert_eq!(ui.sizes.cube_width, 65);
                assert!(ui.embed.is_some());
                assert!(ui.inline_screen.is_none());
                terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
                assert!(ui.inline_screen.unwrap().width > screen.width);
            }
            // q is input, never an accidental application exit while controlling.
            assert!(matches!(
                key_event(
                    &mut ui,
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
                ),
                Step::Continue
            ));
            assert!(ui.embed.is_some());
            // Shrinking the outer terminal must revoke the previously visible region.
            let mut tiny =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
            tiny.draw(|f| view::draw(f, &mut ui)).unwrap();
            assert!(ui.inline_screen.is_none());
            assert!(ui.embed.as_ref().unwrap().origin.is_none());
            assert!(matches!(
                key_event(
                    &mut ui,
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
                ),
                Step::Continue
            ));
            assert!(ui.status.contains("paused"));
            paste(&mut ui, "do not submit\n");
            assert!(ui.status.contains("Paste paused"));
            ui.dialog = Some(Dialog {
                mode: "input".into(),
                secret: true,
                ..Dialog::default()
            });
            paste(&mut ui, "123:fixture_value\r\n");
            assert_eq!(ui.dialog.as_ref().unwrap().input, "123:fixture_value");
            assert!(ui.embed.is_some());
            if layout.grid() {
                // A hidden input surface must not trap ordinary navigation.
                key_event(&mut ui, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
                assert!(ui.embed.is_none(), "{layout:?} must release hidden input");
                assert_eq!(ui.selected_terminal().as_deref(), Some("b"), "{layout:?}");
                assert_eq!(ui.view, layout);
                ui.reveal("a");
                open_embed(&mut ui, &plan);
                key_event(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(
                    ui.embed.is_none(),
                    "Esc cancels input even before the stream connects"
                );
                open_embed(&mut ui, &plan);
            }
            // Detach remains usable even when no screen can be rendered.
            key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
            );
            key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            );
            assert!(ui.embed.is_none());
            if layout.grid() {
                assert_eq!(ui.view, layout);
            }
            assert_eq!(ui.selected_terminal().as_deref(), Some("a"));
            if layout.grid() {
                open_embed(&mut ui, &plan);
                ui.reveal("b");
                assert!(ui.embed.is_none(), "Changing selection releases old input");
                assert_eq!(ui.view, layout);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn inline_connecting_keys_cannot_trigger_actions_or_leave_the_layout() {
        let mut ui = workspace();
        ui.view = View::Cube;
        let plan = json!({"adapter":"herdr-session", "mode":"control", "node":"a",
            "argv":[crate::test_support::path("/bin/sh"), "-c", "read line"]});
        #[cfg(unix)]
        {
            open_embed(&mut ui, &plan);
            for code in [KeyCode::Char('q'), KeyCode::Char('a'), KeyCode::Tab] {
                assert!(matches!(
                    key_event(&mut ui, KeyEvent::new(code, KeyModifiers::NONE)),
                    Step::Continue
                ));
                assert_eq!(ui.view, View::Cube);
                assert_eq!(ui.page, WORKSPACES);
                assert!(ui.embed.is_some());
            }
            key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
            );
            key_event(
                &mut ui,
                KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
            );
            assert_eq!(ui.view, View::Wall);
            assert!(ui.embed.is_none());
            assert_eq!(ui.selected_terminal().as_deref(), Some("a"));
        }
    }

    #[test]
    fn failed_attach_releases_action_lock() {
        for plan in [
            json!({"argv":42}),
            json!({"adapter":"herdr-session","argv":["/nonexistent/daedalus-test-stream"]}),
        ] {
            let mut ui = workspace();
            ui.busy = true;
            ui.action_pending = true;
            open_embed(&mut ui, &plan);
            assert!(!ui.busy && !ui.action_pending);
            assert!(ui.status_error && ui.embed.is_none());
        }
    }

    #[test]
    fn previews_are_requested_only_for_on_screen_terminals_when_idle() {
        let mut ui = workspace();
        ui.view = View::Tiles;
        ui.preview_targets = vec!["a".into()];
        assert!(previews_due(&ui));
        ui.dialog = Some(Dialog::default());
        assert!(!previews_due(&ui));
        ui.dialog = None;
        ui.action_pending = true;
        assert!(!previews_due(&ui));
        ui.action_pending = false;
        // The Tree's Details panel previews its selection the same way.
        ui.view = View::Tree;
        assert!(previews_due(&ui));
        // Focus reads only the other terminals of a split tab it draws.
        ui.view = View::Focus;
        assert!(previews_due(&ui));
        ui.preview_targets.clear();
        assert!(!previews_due(&ui));
        ui.preview_targets = vec!["a".into()];
        ui.view = View::Tree;
        // The same terminal refreshes every two seconds, not more often.
        ui.last_preview = Some(Instant::now());
        ui.preview_sent = ui.preview_targets.clone();
        assert!(!previews_due(&ui));
        // A newly selected one is read as soon as the last read has settled.
        ui.preview_targets = vec!["b".into()];
        assert!(!previews_due(&ui));
        ui.last_preview = Some(Instant::now() - PREVIEW_SETTLE - Duration::from_millis(1));
        assert!(previews_due(&ui));
        ui.preview_targets.clear();
        ui.last_preview = None;
        assert!(!previews_due(&ui));
    }
    #[cfg(windows)]
    #[test]
    fn quit_fixture_worker() {
        let Ok(mode) = std::env::var("DAEDALUS_TEST_WORKER") else {
            return;
        };
        if mode == "hang" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(std::env::var_os("DAEDALUS_TEST_SAVED").unwrap(), line).unwrap();
    }
    #[test]
    fn quitting_lets_the_backend_save_before_it_stops() {
        let folder = std::env::temp_dir().join(format!("daedalus-quit-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let saved = folder.join("saved");
        // A stand-in backend: reads the quit request, takes a moment, saves.
        #[cfg(unix)]
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("read line; sleep 0.2; printf '%s' \"$line\" > \"$1\"")
            .arg("sh")
            .arg(&saved)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli_tests::quit_fixture_worker", "--nocapture"])
            .env("DAEDALUS_TEST_WORKER", "save")
            .env("DAEDALUS_TEST_SAVED", &saved)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut host = Host { child, input };
        host.send(json!({"op":"quit","view":{"mode":"tiles"}}))
            .unwrap();
        assert!(host.wait_exit(QUIT_WAIT));
        drop(host);
        let line = std::fs::read_to_string(&saved).unwrap();
        assert!(line.contains("\"mode\":\"tiles\""), "{line}");
        std::fs::remove_dir_all(&folder).unwrap();
        // A backend that never exits is stopped after the wait.
        #[cfg(unix)]
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli_tests::quit_fixture_worker", "--nocapture"])
            .env("DAEDALUS_TEST_WORKER", "hang")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut host = Host { child, input };
        assert!(!host.wait_exit(Duration::from_millis(50)));
    }
    #[test]
    fn switching_views_is_remembered_once_it_settles() {
        let mut ui = workspace();
        ui.view = View::Tree;
        press(&mut ui, KeyCode::Char(']'));
        assert_eq!(ui.view, View::Tiles);
        assert!(ui.sizes_changed.is_some());
    }
    fn option_key(ui: &mut Ui, code: KeyCode) {
        options_key(ui, KeyEvent::new(code, KeyModifiers::NONE));
    }
    #[test]
    fn bindings_overlay_blocks_paste_and_rest_and_new_workspace_works_without_selection() {
        let mut ui = workspace();
        key_event(&mut ui, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(ui.binding_search.is_some());
        assert!(!may_rest(&ui));
        assert!(matches!(paste(&mut ui, "do not send"), Step::Continue));
        key_event(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let Step::Send(request) = key_event(
            &mut ui,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        ) else {
            panic!("new workspace must be accessible")
        };
        assert_eq!(request["action"], "project.new");
    }
    #[test]
    fn configuration_option_opens_settings_without_changing_preferences() {
        let mut ui = workspace();
        let before = view_state(&ui);
        open_options(&mut ui);
        option_key(&mut ui, KeyCode::Enter);
        assert!(ui.options.is_none());
        assert_eq!(ui.page, 4);
        assert_eq!(before, view_state(&ui));
    }
    #[test]
    fn options_open_with_comma_change_in_place_and_are_remembered() {
        let mut ui = workspace();
        ui.view = View::Tree;
        press(&mut ui, KeyCode::Char(','));
        assert!(ui.options.is_some());
        // Move past Configuration and Terminal order to the Details panel.
        option_key(&mut ui, KeyCode::Down);
        option_key(&mut ui, KeyCode::Down);
        option_key(&mut ui, KeyCode::Right);
        assert!(!ui.prefs.details);
        assert!(ui.sizes_changed.is_some(), "remembered once it settles");
        assert_eq!(view_state(&ui)["details"], "off");
        option_key(&mut ui, KeyCode::Enter);
        assert!(ui.prefs.details);
        // An item's own key works inside Options too, and moves the choice to it.
        option_key(&mut ui, KeyCode::Char('o'));
        assert_eq!(ui.order, Order::Layout);
        assert_eq!(
            Item::ALL[ui.options.as_ref().unwrap().selected],
            Item::Order
        );
        // Home reaches Configuration; Up wraps to the update command.
        option_key(&mut ui, KeyCode::Home);
        option_key(&mut ui, KeyCode::Up);
        option_key(&mut ui, KeyCode::Enter);
        assert!(ui.options.is_none() && ui.updates_requested);
        open_options(&mut ui);
        ui.sizes.details = 70;
        option_key(&mut ui, KeyCode::Char('='));
        assert_eq!(ui.sizes, Sizes::default());
        // Motion off holds everything still; the environment's choice wins.
        let motion = Item::ALL.iter().position(|i| *i == Item::Motion).unwrap();
        ui.options.as_mut().unwrap().selected = motion;
        option_key(&mut ui, KeyCode::Right);
        assert!(!ui.prefs.motion && ui.theme.reduced_motion);
        option_key(&mut ui, KeyCode::Right);
        assert!(ui.prefs.motion && !ui.theme.reduced_motion);
        ui.still = true;
        ui.theme.reduced_motion = true;
        option_key(&mut ui, KeyCode::Right);
        assert!(ui.theme.reduced_motion, "reduced motion stays set");
        option_key(&mut ui, KeyCode::Esc);
        assert!(ui.options.is_none());
        // The key bar choice is remembered too.
        press(&mut ui, KeyCode::Char('?'));
        assert_eq!(view_state(&ui)["keys"], "off");
    }
    #[test]
    fn options_follow_clicks_and_close_on_a_click_outside() {
        let mut ui = workspace();
        open_options(&mut ui);
        let row = |i: usize| ratatui::layout::Rect::new(10, 5 + i as u16, 40, 1);
        ui.option_hits = (0..Item::ALL.len()).map(|i| (row(i), i)).collect();
        ui.option_panel = ratatui::layout::Rect::new(8, 3, 50, 20);
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let pinned = Item::ALL.iter().position(|i| *i == Item::Pinned).unwrap();
        mouse_event(&mut ui, click(12, 5 + pinned as u16));
        assert!(!ui.prefs.pinned);
        assert_eq!(ui.options.as_ref().unwrap().selected, pinned);
        mouse_event(&mut ui, click(9, 4));
        assert!(ui.options.is_some(), "inside the panel, between rows");
        mouse_event(&mut ui, click(100, 30));
        assert!(ui.options.is_none());
    }
    #[test]
    fn a_terminal_in_the_sidebar_is_selected_then_opened() {
        let mut ui = workspace();
        ui.page = 2;
        ui.view = View::Tiles;
        ui.pinned_only = true;
        ui.side_hits = vec![(ratatui::layout::Rect::new(1, 20, 20, 1), "a".into())];
        let click = MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: 3,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        // a is not pinned: the filter gives way so it can be selected.
        assert!(matches!(mouse_event(&mut ui, click), Step::Continue));
        assert_eq!(ui.page, WORKSPACES);
        assert!(!ui.pinned_only);
        assert_eq!(ui.selected_terminal().as_deref(), Some("a"));
        let step = mouse_event(&mut ui, click);
        assert_eq!(ui.view, View::Focus);
        assert!(matches!(step, Step::Send(v) if v["action"] == "terminal.watch"));
    }
    #[test]
    fn question_mark_toggles_the_shortcut_bar_but_not_while_searching() {
        let mut ui = workspace();
        press(&mut ui, KeyCode::Char('?'));
        assert!(ui.hide_keys);
        press(&mut ui, KeyCode::Char('?'));
        assert!(!ui.hide_keys);
        ui.searching = true;
        press(&mut ui, KeyCode::Char('?'));
        assert!(!ui.hide_keys);
        assert!(ui.filter.ends_with('?'));
    }
    #[test]
    fn number_keys_follow_the_sidebar_and_zero_is_the_tenth_page() {
        let mut ui = workspace();
        assert_eq!(ui.page_count(), COMMANDS + 1);
        for (key, page) in [('9', 8), ('0', 9), ('1', 0), ('6', 5)] {
            press(&mut ui, KeyCode::Char(key));
            assert_eq!(ui.page, page, "key {key}");
        }
        // Commands keeps Ctrl+P; no number key reaches it.
        assert!((0..10).all(|d| digit_page(char::from(b'0' + d)) < COMMANDS));
    }
    #[test]
    fn configuration_value_is_not_interpreted_as_a_subcommand() {
        let (setup, args) =
            parse_args(vec!["tui".into(), "--config-dir".into(), "setup".into()]).unwrap();
        assert!(!setup);
        assert_eq!(args, vec!["--config-dir", "setup"]);
        assert!(parse_args(vec!["--config-dir".into()]).is_err());
        assert!(parse_args(vec!["--unknown".into()]).is_err());
        // One release number: the repository's VERSION file and this crate.
        assert_eq!(
            include_str!("../../../VERSION").trim(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn native_state_deserializes_into_unchanged_tui_model() {
        use daedalus_tui::app::{
            backend, config::ControlConfig, presentation::Context, tree::ProjectionContext,
        };
        let fixture: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/tree-projection.json"))
                .unwrap();
        let config = ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap();
        let local = json!({});
        let context = Context {
            config: &config,
            config_dir: std::path::Path::new(crate::test_support::path("/tmp/example")),
            home: None,
            snapshot: &fixture["snapshot"],
            health: &fixture["health"],
            local: &local,
            layouts: None,
            show_saved: true,
            demo: false,
            refresh_error: false,
            pending: false,
            last_success: Some(1120.0),
            now: 1120.0,
            telegram_token_saved: Some(false),
        };
        let projection = ProjectionContext {
            hostname: "fixture-host".into(),
            home: crate::test_support::path("/home/example").into(),
            now: 1120.0,
            pins: fixture["context"]["pins"].as_array().unwrap().clone(),
        };
        let frame = backend::assemble(&context, &projection, 0, "", true, true).unwrap();
        let expected_nodes = frame["tree"].as_array().unwrap().len();
        let snapshot: Snapshot = serde_json::from_value(frame).unwrap();
        assert_eq!(snapshot.sections.len(), 11);
        assert_eq!(snapshot.terminal_actions.len(), 17);
        assert_eq!(snapshot.tree.len(), expected_nodes);
        assert!(expected_nodes > 0);
        assert!(!snapshot.catalog.is_empty());
        assert!(!snapshot.overview.connections.is_empty());
    }
    fn focus_with_divider() -> Ui {
        let mut ui = workspace();
        model::tests::split(&mut ui);
        ui.view = View::Focus;
        ui.focus_target = "a".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|frame| view::draw(frame, &mut ui)).unwrap();
        assert_eq!(ui.pane_dividers.len(), 1);
        ui
    }
    fn divider_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn focus_divider_drag_previews_then_submits_one_exact_native_resize() {
        let mut ui = focus_with_divider();
        let original = ui.tab_layout("a").unwrap().clone();
        let edge = ui.pane_dividers[0].clone();
        let (x, y) = (edge.area.x, edge.area.y + 1);
        assert!(matches!(
            mouse_event(
                &mut ui,
                divider_event(MouseEventKind::Down(event::MouseButton::Left), x, y)
            ),
            Step::Continue
        ));
        assert!(matches!(
            mouse_event(
                &mut ui,
                divider_event(MouseEventKind::Drag(event::MouseButton::Left), x + 10, y)
            ),
            Step::Continue
        ));
        assert_eq!(
            ui.tab_layout("a"),
            Some(&original),
            "drag does not fake a changed backend layout"
        );
        assert!(ui.pane_resize.as_ref().unwrap().cells > 0);
        let step = mouse_event(
            &mut ui,
            divider_event(MouseEventKind::Up(event::MouseButton::Left), x + 10, y),
        );
        let Step::Send(request) = step else {
            panic!("release must submit once")
        };
        assert_eq!(request["action"], "terminal.resize");
        assert_eq!(request["node"], "a");
        assert_eq!(request["resize"]["peer"], "b");
        assert_eq!(request["resize"]["axis"], "horizontal");
        assert_eq!(
            request["resize"]["layout"],
            serde_json::to_value(original).unwrap()
        );
        assert_eq!(request["resize"]["cells"], edge.cells((x, y), (x + 10, y)));
        assert!(ui.pane_resize.is_none());
        assert!(matches!(
            mouse_event(
                &mut ui,
                divider_event(MouseEventKind::Up(event::MouseButton::Left), x + 10, y)
            ),
            Step::Continue
        ));
    }
    #[test]
    fn focus_resize_cancellation_and_changed_layout_never_send_mutations() {
        let mut ui = focus_with_divider();
        let edge = ui.pane_dividers[0].clone();
        let (x, y) = (edge.area.x, edge.area.y + 1);
        mouse_event(
            &mut ui,
            divider_event(MouseEventKind::Down(event::MouseButton::Left), x, y),
        );
        key_event(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.pane_resize.is_none());
        assert!(matches!(
            mouse_event(
                &mut ui,
                divider_event(MouseEventKind::Up(event::MouseButton::Left), x + 10, y)
            ),
            Step::Continue
        ));
        mouse_event(
            &mut ui,
            divider_event(MouseEventKind::Down(event::MouseButton::Left), x, y),
        );
        ui.snapshot.tree[0].layout.as_mut().unwrap().panes[0].w -= 1;
        assert!(matches!(
            mouse_event(
                &mut ui,
                divider_event(MouseEventKind::Up(event::MouseButton::Left), x + 10, y)
            ),
            Step::Continue
        ));
        assert!(ui.status.contains("layout changed"));
    }
    #[test]
    fn focus_alt_arrows_resize_real_divider_instead_of_details_panel() {
        let mut ui = focus_with_divider();
        let sizes = ui.sizes;
        let Step::Send(request) =
            key_event(&mut ui, KeyEvent::new(KeyCode::Right, KeyModifiers::ALT))
        else {
            panic!("Alt+Right must resize the split")
        };
        assert_eq!(request["resize"]["cells"], 2);
        assert_eq!(ui.sizes, sizes);
        ui.focus_target = "b".into();
        let Step::Send(request) =
            key_event(&mut ui, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT))
        else {
            panic!("Alt+Left must use the left divider")
        };
        assert_eq!(request["node"], "a");
        assert_eq!(request["resize"]["cells"], -2);
        ui.zoomed = true;
        assert!(matches!(
            key_event(&mut ui, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
            Step::Continue
        ));
    }
    #[test]
    fn wall_presets_clear_stale_filters_and_focus_tree_clicks_open_exact_panes() {
        let mut ui = workspace();
        ui.filter = "old filter".into();
        ui.pinned_only = true;
        press(&mut ui, KeyCode::Char('B'));
        assert_eq!(ui.view, View::Wall);
        assert!(ui.working_only && !ui.pinned_only && ui.filter.is_empty());
        press(&mut ui, KeyCode::Char('W'));
        assert!(!ui.working_only && !ui.pinned_only);
        ui.view = View::Focus;
        ui.focus_target = "a".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (rect, _, branch) = ui
            .focus_tree_hits
            .iter()
            .find(|(_, id, _)| id == "b")
            .cloned()
            .unwrap();
        assert!(!branch, "a pane with an agent child must remain clickable");
        let step = mouse_event(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let Step::Send(request) = step else {
            panic!("expected watch request")
        };
        assert_eq!(request["node"], "b");
        assert_eq!(request["action"], "terminal.watch");
        assert_eq!(ui.focus_target, "b");
        assert!(ui.focus_tree_reveal);
    }
    #[test]
    fn crowded_wall_click_selects_then_double_click_attaches_in_place() {
        let mut ui = workspace();
        ui.view = View::Wall;
        let template = ui.snapshot.tree[1].clone();
        for i in 0..26 {
            let mut node = template.clone();
            node.id = format!("fleet-{i}");
            ui.snapshot.tree.push(node);
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 44)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        assert!(ui.wall_roster);
        let (rect, index) = *ui
            .tile_hits
            .iter()
            .find(|(_, index)| *index > 0 && !ui.tiles()[*index].id.is_empty())
            .unwrap();
        let event = MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(mouse_event(&mut ui, event), Step::Continue));
        assert_eq!(ui.view, View::Wall);
        assert_eq!(ui.tile, index);
        let Step::Send(request) = mouse_event(&mut ui, event) else {
            panic!("attach request")
        };
        assert_eq!(request["action"], "terminal.attach");
        assert_eq!(request["node"], ui.tiles()[index].id);
        assert_eq!(ui.view, View::Wall);
    }
    #[test]
    fn click_outside_a_terminal_detaches_on_wall_and_tiles() {
        for view in [View::Wall, View::Tiles] {
            let mut ui = workspace();
            ui.set_view(view);
            ui.reveal("a");
            let plan = json!({
                "adapter":"herdr-session", "mode":"control", "node":"a",
                "argv":[crate::test_support::path("/bin/sh"), "-c", "while read line; do :; done"]
            });
            open_embed(&mut ui, &plan);
            assert!(ui.embed.is_some());
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 44)).unwrap();
            terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
            let blank = (0..44)
                .flat_map(|row| (0..160).map(move |column| (column, row)))
                .find(|&(column, row)| {
                    let point = ratatui::layout::Position::new(column, row);
                    !ui.tile_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.pane_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.nav_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.side_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.view_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.quick_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.list_hits.iter().any(|(rect, _)| rect.contains(point))
                        && !ui.handles.iter().any(|handle| handle.area.contains(point))
                })
                .unwrap_or_else(|| panic!("no empty cell in {view:?}"));
            let click = MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: blank.0,
                row: blank.1,
                modifiers: KeyModifiers::NONE,
            };
            assert!(matches!(mouse_event(&mut ui, click), Step::Continue));
            assert!(ui.embed.is_none(), "{view:?} stays attached");
            assert_eq!(ui.view, view);
            assert!(ui.status.contains("Detached"));
        }
    }
    #[test]
    fn f11_fills_the_screen_and_returns_to_the_wall() {
        let mut ui = workspace();
        ui.view = View::Wall;
        ui.reveal("a");
        let Step::Send(request) =
            key_event(&mut ui, KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE))
        else {
            panic!("fullscreen watches the selected terminal");
        };
        assert_eq!(request["action"], "terminal.watch");
        assert_eq!(ui.view, View::Focus);
        assert!(ui.zoomed);
        assert_eq!(ui.fullscreen_from, Some(View::Wall));
        assert!(matches!(
            key_event(&mut ui, KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE),),
            Step::Continue
        ));
        assert_eq!(ui.view, View::Wall);
        assert!(!ui.zoomed);
        assert!(ui.fullscreen_from.is_none());
        assert!(ui.status.contains("Left fullscreen"));

        ui.view = View::Focus;
        ui.previous = View::Tiles;
        ui.focus_target = "a".into();
        ui.zoomed = false;
        key_event(&mut ui, KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE));
        assert_eq!(ui.view, View::Focus);
        assert!(ui.zoomed);
        key_event(&mut ui, KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE));
        assert_eq!(ui.view, View::Focus);
        assert!(!ui.zoomed);
    }
    #[test]
    fn focus_tree_disclosure_toggles_pane_children_and_session_click_targets_parent() {
        let mut ui = workspace();
        ui.view = View::Focus;
        ui.focus_target = "a".into();
        ui.snapshot
            .tree
            .iter_mut()
            .find(|n| n.id == "b")
            .unwrap()
            .children = true;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 50)).unwrap();
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let (rect, _) = ui
            .focus_tree_toggles
            .iter()
            .find(|(_, id)| id == "b")
            .cloned()
            .unwrap();
        let event = MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(mouse_event(&mut ui, event), Step::Continue));
        assert!(ui.collapsed.contains("b"));
        assert_eq!(ui.focus_target, "a");
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        assert!(!ui.focus_tree_hits.iter().any(|(_, id, _)| id == "b-agent"));
        assert!(matches!(mouse_event(&mut ui, event), Step::Continue));
        terminal.draw(|f| view::draw(f, &mut ui)).unwrap();
        let rect = ui
            .focus_tree_hits
            .iter()
            .find(|(_, id, _)| id == "b-agent")
            .unwrap()
            .0;
        let Step::Send(request) = mouse_event(
            &mut ui,
            MouseEvent {
                column: rect.x,
                row: rect.y,
                ..event
            },
        ) else {
            panic!("session must open parent pane read-only")
        };
        assert_eq!(request["node"], "b");
        assert_eq!(request["action"], "terminal.watch");
        assert_eq!(ui.focus_target, "b");
    }
}
