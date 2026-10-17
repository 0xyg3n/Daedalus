# Control panel

Run `daedalus` to open the panel. The first launch opens setup. The panel runs the
same executable as a local backend (`daedalus backend --stdio`) and never acts on a
terminal it cannot identify exactly.

## Pages

| Group | Pages |
|---|---|
| Work | Overview, Workspaces, Sessions |
| Connect | Telegram, AI tools, Hooks |
| System | Activity, Instances, Armonia (optional), Help |

Tab or 0-9 switches pages, Ctrl+P searches every command, `?` shows the key bar and
`q` quits. The Details panel explains the selected item. Help includes the
[command reference](COMMANDS.md).

- **AI tools**: sign-in, profile directories and update checks for the seven tools.
- **Hooks**: hook profiles and Hindsight memory ([hooks](HOOKS.md)).
- **Telegram**: bot configuration, routes, feeder registration and reply recipients.
  Each is a separate explicit step; sender permissions are separate from routes.
- **Activity**: what happened, and whether each step was attempted, queued,
  submitted, delivered or uncertain.
- **Instances**: saved configurations. One is shown as running only when its
  service is verified.

## Workspaces

Daedalus lists tmux (servers, sessions, windows, panes) and Herdr (servers,
workspaces, tabs, panes) side by side, including Herdr clients running inside tmux.
Set nondefault tmux sockets and Herdr endpoints in the configuration. Keep each
project in its own workspace with its shells, agents and logs.

| View | Shows |
|---|---|
| Tree | The full inventory |
| Tiles | Live previews of the visible terminals, with ANSI colors |
| Wall | The selected terminals fitted to the screen |
| Focus | One terminal, watched or attached |

| Key | Action |
|---|---|
| `[` `]` | Previous / next view |
| `z` | Toggle Focus |
| `/` | Search |
| `o` | Switch between latest-activity and layout order |
| Enter, `m` | Terminal menu |
| `w` | Watch (read-only) |
| `a` | Attach and type; Ctrl+] then D detaches and the terminal keeps running |
| `f` | Fork the agent conversation |
| `d` | Duplicate the view of the same terminal |
| `p`, `P` | Pin to Tiles; show pinned only |
| `c` | Save a checkpoint |
| `t` | Route a Telegram chat to this terminal |
| `L` | Save the layout |
| `r` | Refresh |

The terminal menu also offers checkpoint details and removal, resume, cross-tool
handoff, layouts and terminal controls. Fork starts a new conversation; duplicate
view shows the same process. Resume and fork appear only when the installed tool
supports them ([harnesses](HARNESSES.md#sessions)). Layouts, pins and workflows are
saved in the configuration directory. Previews read recent output of visible
terminals only and are not stored. Watching, attaching and capture can show private
terminal output.

## Reviews

Every change opens a review dialog. A confirmation applies once, to the exact
target shown. If the terminal, process or configuration changed in the meantime,
it is rejected. PgUp/PgDn scroll long reviews; Up/Down choose the action.

## Armonia

Armonia is an optional agent-identity registry. When one is configured, Daedalus
reads its stable numeric IDs and aliases and can rename agents without changing
them (`daedalus agent-identity`, Telegram `/rename`). Organizer status is shown only
when its process can be verified. Renaming Herdr metadata needs an explicit socket
mapping.

## Display

| Setting | Effect |
|---|---|
| `NO_COLOR=1` | No color |
| `DAEDALUS_ICONS=ascii` | Plain-character icons |
| `DAEDALUS_REDUCED_MOTION=1` | No animation |
| `DAEDALUS_BACKDROP=0` | No background light or artwork; otherwise `b` changes it |
| `DAEDALUS_SPLASH=0` | Skip the start-up splash |
| `DAEDALUS_SCREENSAVER=SECONDS` | Idle time before the screensaver (`0`: never); `s` toggles it |
| `DAEDALUS_GRAPHICS=0` or `kitty` | Force the character artwork, or always request images |

Terminals with the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
show the start-up splash as a full-resolution image; other terminals get a
character version. Inside Herdr, enable `kitty_graphics`. A key press, click,
scroll or paste removes the image. The screensaver is always glitch art in
character cells: scanlines, a color split, a rolling light and short glitch bursts.

## Resize panes in Focus

When Focus shows a split tab, drag a shared pane border marked with **↔** or
**↕**. An accent guide previews the new boundary; release applies one resize to
the actual tmux/Herdr panes. **Esc** cancels. The original layout stays visible
until the backend reports its updated geometry.

In Observe mode, **Alt+Left/Right** moves the nearest selected vertical divider
two native columns; **Alt+Up/Down** moves a horizontal divider one native row.
While attached, use **Ctrl+]**, then **Alt+arrow** so ordinary terminal keys keep
going to the application. Zoom out with **Z** to expose split dividers.

This changes the backend's shared pane layout, so other views of that tab see the
same sizes. It does not resize only Daedalus's Details panel. Both exact terminal
identities and the captured layout are checked again before the native command;
a changed layout is rejected and an uncertain result is not automatically retried.
Small-pane limits and backend resize support still apply.

## Quick terminal switcher and Herdr usage

Press **Ctrl+K** from any page to search live terminals by workspace, tab, pane,
agent or displayed details. Pinned live panes come first, then recent Herdr
selections. Arrow keys select; **Enter** opens a read-only watch inside Daedalus.
**Alt+1–9** opens the corresponding visible result. **Esc** returns to the tree.
While attached, **Ctrl+] then K** opens the switcher without sending Ctrl+K to
the agent. Use the existing attach control when you want to type.

Herdr selection counts and sampled selected time appear in terminal Details and
the switcher. These are **server selection observations**, not per-client viewing,
human attention, AI tokens, billing or subscription limits. They accumulate only
while the Daedalus backend refreshes. Gaps over 30 seconds, disconnected discovery,
changed terminal generations and process restarts do not accrue time. Switching
intervals are conservatively excluded because the exact switch time is unknown.

History is stored privately in `<config-dir>/workspace-usage.sqlite3`. It contains
hashed terminal/server identities, counters and timestamps, with no pane text,
workspace paths, prompts or credentials. Instance import copies these local
aggregates without activating tracking during preview. Configuration and runtime
databases stay outside Git. Existing pin ordering is the shortcut priority; no
background monitoring service is installed.
