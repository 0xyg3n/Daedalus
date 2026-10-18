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
| Tiles | Selected workspace: a large terminal and companion previews |
| Wall | Fleet overview, workspace badges and a selected terminal inspector |
| Cube | One workspace per cube; rotate previews or type on the front face |
| Focus | A terminal and its native tab layout beside the workspace tree |

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
| `DAEDALUS_SCREENSAVER=SECONDS` | Idle time before the screensaver (default: 15 seconds; `0`: never); `s` toggles it |
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

## Workspace overview and updates

Focus keeps a scrollable workspace outline beside the terminal (or above it on
narrow screens). Click a terminal to watch it; its previous terminal keeps running.
Branches collapse independently. Small terminals retain the terminal-only view.

In Workspaces, **W** opens all terminals in Wall and **B** limits the Wall to
terminals reporting working or blocked. The **All / Working / Pinned** buttons
switch scope. Working terminals come first in activity order; pinned-only mode
preserves pin order. Crowded walls use a readable terminal list with workspace
badges, status labels and one large selected screen. Click a row to select it;
double-click to attach in place. Long lists keep the selection visible. Only that selected
screen is captured in this mode, reducing background terminal reads.

**Ctrl+U** opens release details. While attached, use **Ctrl+]**, then **U**.
Updates require explicit approval; the startup check can be disabled with
`DAEDALUS_UPDATE_CHECK=0`. See [installation](INSTALL.md#update-and-removal).

Tiles uses the full workspace width for one large selected screen and a paged
strip of companion terminals from the same workspace. `+` and `-` adjust companion
size; PageUp/PageDown selects the next group. Workspace tabs and Alt+Up/Down
switch workspaces in Tiles and Wall. Wall uses compact previews for the overview and, on wide terminals,
a separate enlarged screen for the selected terminal. Its overview cards stay
compact even when only a few terminals exist. Focus shows harness icons and
explicit colored status labels in its persistent workspace outline.

Terminal status labels use green for working, amber for blocked, muted text for
idle, rose for errors and faint text for unknown state. Status and recency are
separate: a recently selected terminal is not necessarily working. Focus and
sidebar entries show these labels, and terminal cards use the same state colors.
The local action bar owns Attach/Watch shortcuts; the footer supplies navigation
and only adds an action shortcut when it could not fit in the local bar.

Focus separates human session names from identity details. Terminal entries show
harness, pane, short conversation ID and reported profile/model text beneath
the name when space permits; the selected entry can add its reported topic.
Unknown details are omitted. Hover a terminal for its full name and metadata.
The selected terminal's folder and session context sit above its screen, and
Workspace Tree expands the selected terminal's metadata on a separate line.
Focus adjusts sidebar width while reserving at least 50 columns for the terminal;
short or narrow outlines fall back to compact entries. Scrolling reveals whole
entries so a selected session's detail lines stay together.

Focus uses the same full node hierarchy, ordering, search and collapse state as
Workspace Tree, including agent-session children and other reported nodes.
Click a disclosure arrow to expand or collapse children; clicking the pane name
watches that pane. Clicking an agent-session child watches its parent pane.
Hierarchy connectors continue through the extra metadata lines.

## Type without changing layouts (source builds from main)

In Cube, Tiles and Wall, select a terminal and press **a**, double-click its card,
or click inside its selected screen. Input and live output stay in that layout;
Daedalus does not substitute the backend's native split layout. **w** opens a
read-only stream in the same place. **z** explicitly opens Focus.

The selected screen shows **INPUT HERE** while controlling. Ordinary keys go to
that terminal. Press **Ctrl+]**, then **D** to detach, **O** to observe, or **[** / **]**
to switch layout. In preview layouts, **Ctrl+]** followed by an arrow detaches and
navigates using that layout's directions. Selecting another terminal stops the old
stream; it does not silently send input to the new terminal. Click its screen or
press **a** when ready to type there. Detached terminal processes keep running.

Input pauses if the window is too small to show the selected screen. Enlarge the
window or use **Ctrl+] then D** to detach. tmux keeps its native size by default
and follows the cursor inside the available viewport; **Ctrl+] then F** explicitly
enables size fitting, which affects other viewers of that tmux window. Herdr's
controller sizes the selected terminal to its visible screen.

## Resize the Cube front terminal

Drag the front face's **right edge (↔)** to change its width, or its **bottom edge
(↕)** to change its height. The opposite edge moves symmetrically, keeping the
cube centred. Double-click an edge to reset that dimension. **< / >** adjusts
width; **+ / -** adjusts height. While typing, press **Ctrl+]** before the resize
key. **=** resets panel sizes. Cube proportions are saved with your local layout.

Neighbours stay outside the front face. Small windows show only the front
terminal; saved proportions return when there is room. This changes the Cube
view; Herdr controllers follow its viewport, while tmux only follows it when
Fit size is enabled. Unused space carries a circuit pattern with moving lights
and brief glitch fragments. Reduced motion keeps the pattern stationary.
