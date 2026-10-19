# Installation

Daedalus is one native executable: `daedalus` on Linux x86_64/ARM64 or
`daedalus.exe` on Windows x64. It provides the TUI, CLI, Telegram service and
hook runtime. Agent tools keep their own runtimes and accounts. macOS is not
supported. Windows support is implemented but has not been validated on real
Windows hardware; run the local checks below before deploying it.

## Linux install

```bash
git clone https://github.com/0xyg3n/Daedalus.git
bash Daedalus/install.sh --source "$PWD/Daedalus"
~/.local/bin/daedalus setup
```

The installer builds the committed `HEAD` of `--source`; uncommitted edits are not
installed. It stores the binary at `PREFIX/share/daedalus/native/SHA256/daedalus`
and links `PREFIX/bin/daedalus` to it. The default prefix is `~/.local`
(`DAEDALUS_PREFIX` overrides it).

| Option | Effect |
|---|---|
| `--plan` | Print the plan and exit without changes |
| `--yes` | Accept the plan without prompting |
| `--prefix ABS` | Install under another prefix |
| `--ref REF` | Build this revision. Without `--source`, the installer clones `main` (or `DAEDALUS_REF`) |
| `--no-deps` | Fail instead of installing missing build tools |
| `--binary ABS --sha256 HEX` | Install a prebuilt binary after checking its checksum and ELF architecture |

Building needs Rust 1.88+, a C linker, Git and curl. After you approve, the
installer adds missing build tools with apt, dnf or pacman, and Rust with the
official rustup script (it prints the script's SHA-256 first). It never replaces a
`daedalus` command it did not install. A checksum proves integrity only if you got
it from a source you trust. ARM64 builds have not been tested on real hardware.

## Native Windows install

Download **Daedalus-0.2.0-beta.10-setup.exe** from the [current release](https://github.com/0xyg3n/Daedalus/releases/tag/v0.2.0-beta.10), then double-click it. The self-contained installer needs no PowerShell, Git or compiler. It installs to `%LOCALAPPDATA%\Daedalus`, creates a Start Menu shortcut and opens the TUI. On first run, use its setup flow. The built-in terminal backend needs no separate multiplexer. The installer has no admin requirement and leaves private configuration in place when uninstalled. The release also includes a standalone executable and SHA-256 checksums for advanced users. Stable update checks do not select prereleases.


The built-in **Daedalus native terminals** backend uses ConPTY; Herdr is optional.
Native Windows multiplexers such as [tmux-windows](https://github.com/arndawg/tmux-windows)
and [PSMux](https://github.com/psmux/psmux) can be selected through explicitly
configured compatible endpoints; command and identity contracts require validation
for the installed build. Unix socket discovery is not assumed on Windows. WSL
sessions are a separate environment. Set `DAEDALUS_TMUX_BIN` to the absolute
path of a compatible executable when selection is needed; discovery otherwise
tries `tmux.exe`, then `psmux.exe`. PSMux `.psmux/*.port` namespace markers are
automatic candidates; configured absolute `tmux_sockets` endpoints are passed
through `-S` for the selected implementation. Every candidate still needs fresh
identity evidence before mutation. For the optional Herdr adapter, install a Windows-compatible Herdr build. Install the Windows versions of the harnesses you choose. Provider support depends on
each upstream CLI; Linux shell installers are not run on Windows.

For an optional source build, use a reviewed checkout in native PowerShell:

```powershell
git clone https://github.com/0xyg3n/Daedalus.git
.\Daedalus\install.ps1 -Source (Resolve-Path .\Daedalus).Path -Plan
.\Daedalus\install.ps1 -Source (Resolve-Path .\Daedalus).Path
& "$env:LOCALAPPDATA\Daedalus\bin\daedalus.exe" setup
```

Source builds require Git, Rust 1.88+ with the MSVC target and Visual Studio C++
Build Tools. Missing prerequisites are listed with exact `winget` commands.
`-InstallDependencies` explicitly allows their reviewed installation; reopen
PowerShell afterwards to refresh the toolchain environment. No dependencies are
installed by `-Plan` or `-WhatIf`.

To install an independently verified Windows executable instead:

```powershell
.\install.ps1 -Binary C:\Downloads\daedalus.exe -Sha256 EXPECTED_64_HEX_SHA256 -Plan
.\install.ps1 -Binary C:\Downloads\daedalus.exe -Sha256 EXPECTED_64_HEX_SHA256
```

Alternatively use `-DownloadUrl HTTPS_URL -Sha256 EXPECTED_64_HEX_SHA256`. Supply
an actual published direct-download URL and a checksum from a trusted channel;
the installer does not assume a Windows artifact exists for an older release.
Redirects and URLs with embedded credentials are rejected. Checksums prove byte
integrity, not publisher identity when both come from an untrusted source.

The PowerShell source installer checks SHA-256 and PE x64 architecture, keeps versioned binaries
under `PREFIX\share\daedalus\native\SHA256`, and copies the selected binary to
`PREFIX\bin\daedalus.exe`. It restricts the installed executable and its directory to the current Windows user, as required by the updater. If an older installation reports unsafe update directory permissions, close Daedalus and rerun the installer from the verified release binary. A standalone executable run directly from Downloads should be installed before using in-app updates. The default prefix is `%LOCALAPPDATA%\Daedalus`;
`-Prefix ABS` overrides it. Existing commands without an installer receipt, or
whose installed bytes changed, are not overwritten. A running executable may
need to be closed before replacement. PATH is not changed automatically.

Initial Windows support targets x64. ARM64 requires explicit `-AllowEmulation`
for x64 emulation, which is unverified; no native ARM64 package is claimed.
Installation never starts a bot, service, private setup or provider sign-in.
Guided first launch with hooks is unavailable on Windows. Existing verified
Herdr processes have native hook contracts for Claude Code 2.1.296+, Hermes, Pi
and OpenCode. Codex/Grok/Cursor hooks and hooks through native tmux-compatible
backends remain unverified and fail closed. Herdr duplicate attachment views
are unavailable; use Watch. Remote feeding still requires exact process,
terminal generation and conversation registration. See the [Windows capability
boundaries](HARNESSES.md#native-windows-boundaries); live provider compatibility
is not established by this port.

### Optional native backend smoke test

The standard local check does not start terminal servers. To explicitly test an
installed native backend with one isolated echo fixture:

```powershell
.\scripts\check-windows.ps1 -BackendSmoke PSMux -BackendBinary C:\Tools\psmux.exe
.\scripts\check-windows.ps1 -BackendSmoke Herdr -BackendBinary C:\Tools\herdr.exe
```

Or run only the backend test against a selected built/installed executable:

```powershell
.\scripts\check-windows-backends.ps1 -Backend PSMux -BackendBinary C:\Tools\psmux.exe -DaedalusBinary C:\Tools\daedalus.exe
```

The opt-in test creates a GUID-named private empty project and its own PSMux
endpoint or Herdr session. It compiles a small native echo program with an
already installed .NET Framework C# compiler; no package installation, shell
profile, harness, sign-in, Telegram request or provider connection is used.
Herdr background version/manifest checks and agent restoration are disabled in
its isolated configuration. No live default multiplexer endpoint is selected.

It uses native configuration and inventory to obtain an exact terminal target,
checks inert send preview, sends a generated marker, presses Enter, captures the
echo response and rejects a deliberately stale generation. Cleanup targets only
the created endpoint/session and uniquely owned fixture processes. Failed
cleanup retains the private fixture directory and reports its name for review.
This script has parser validation here; successful native backend execution must
still be established on Windows. Wine/unit results do not establish a real
Herdr/PSMux installation or ConPTY behavior.

### Private Windows state

Native Windows databases use `TRUNCATE` rollback journals with explicitly private
precreated journal files and in-memory temporary storage. Existing WAL/SHM files
must pass the same owner/ACL checks before opening; their data is not deleted to
force migration. A database whose journaling mode cannot safely change is refused.
This avoids relying on filesystem ACL inheritance for SQLite-created sidecars.

### Windows local validation

```powershell
.\scripts\check-windows.ps1
```

This runs format, tests, Clippy and release build plus isolated installer fixtures
for read-only planning, PE/checksum validation and changed-file protection. It
uses no real bots or paid providers. It is a local workflow and creates no GitHub
Actions billing. Linux cross-compilation alone does not establish Windows console,
ConPTY, ACL, Herdr or provider behavior; real Windows validation remains required.

## First run

`daedalus setup`, or the first `daedalus` launch, opens setup in the TUI.
`daedalus setup --cli` asks the same questions as prompts. Setup:

1. shows your platform and defaults to built-in native terminals; optional tmux/Herdr dependencies are selected separately;
2. selects agent tools and lists the missing ones it would install;
3. writes a private configuration directory (0700 on Unix; owner-scoped access on Windows).

Each tool signs in through its own login flow. Cancelling setup saves nothing.
**Finish and explore terminals** saves the configuration without installing
anything or signing in. Setup never starts the Telegram service.

On Linux, missing tools are installed only after review:

- tmux, Node.js and npm: the system package manager.
- Claude Code, Codex, Pi and OpenCode: npm, into `~/.local`.
- Herdr, Cursor, Grok and Hermes: their official install scripts over HTTPS.

Daedalus also finds OpenCode in `~/.opencode/bin`. Upstream installers are
third-party code that Daedalus does not control.

Next, open `daedalus` and start a shell or agent in a workspace ([control panel](TUI.md)).
Telegram is configured separately ([commands](COMMANDS.md#telegram)).

## Configuration directory

Daedalus uses the first of: `--config-dir`, `DAEDALUS_CONFIG_DIR`,
`$XDG_CONFIG_HOME/daedalus`, then the platform default (`%APPDATA%\Daedalus`
on Windows, `~/.config/daedalus` on Linux). Keep it outside Git. The files
are described in [config/README.md](../config/README.md).

To copy an existing instance into a new directory, run
`daedalus config import-instance --config-dir NEW --from OLD` (preview; `--apply`
writes, `--credentials` also copies token files). If the old instance ran the
Telegram service, stop it and hand over with `daedalus telegram cutover preview|apply`.

## Official component sources

| Component | Installer or distribution reference |
|---|---|
| tmux | [github.com/tmux/tmux](https://github.com/tmux/tmux/wiki/Installing) |
| Herdr | [herdr.dev](https://herdr.dev/) |
| Claude Code | [Anthropic setup](https://code.claude.com/docs/en/setup) |
| Codex | [OpenAI Codex](https://github.com/openai/codex) |
| Grok CLI | [xAI build documentation](https://docs.x.ai/build/overview) |
| Hermes Agent | [Nous Research documentation](https://hermes-agent.nousresearch.com/docs/) |
| Cursor | [Cursor CLI](https://cursor.com/docs/cli/overview) |
| Pi | [Pi installation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md) |
| OpenCode | [OpenCode installation](https://opencode.ai/docs/) |
| Node.js | [Official distributions and checksums](https://nodejs.org/dist/) |

## Update and removal

The TUI checks releases in the background at launch and before exit. A newer compatible release
opens a prompt when no terminal or dialog owns input; **Later** is selected by
default. **Options → Application → Check for updates** or **Ctrl+U** checks again
and opens update details (**Ctrl+]**, then **U**
while attached). A declined version stays in the status bar. There are no periodic checks.
On exit, **Exit** is the default; **Esc** can skip a slow check. If you choose
**Update**, Daedalus stays open to show the installation result.
The screensaver yields to an update prompt; terminal input and other dialogs keep
priority. **R** retries a check from the update dialog.
Set `DAEDALUS_UPDATE_CHECK=0` to disable automatic checks; manual checks still work. Stable installations
select stable releases; beta installations can select newer betas or stable releases.

`daedalus update check --json` returns a review with version, platform, checksum
and approval digest. It installs nothing. Explicit installation uses:

```sh
daedalus update install --version VERSION --approve APPROVAL_SHA256
```

The installer rechecks the release and approval against the running binary,
verifies SHA-256 and native architecture, and preserves the previous executable.
Linux activation is atomic; the running session continues until you restart it.
After installation, a confirmation shows the installed version and restart
instructions, even if you closed the dialog while it was downloading. A status
notice remains visible until restart. Failed checks and installations also show
a notice with details available through **Ctrl+U**.
Windows downloads and verifies a staged executable but does **not** activate it:
close the application and follow the returned manual activation instructions.
Neither path restarts Telegram or changes private configuration.

On Windows, close Daedalus and stop its Telegram service before deleting its
installation prefix; private configuration remains separate.

On Linux, stop its Telegram service, then delete `PREFIX/bin/daedalus`
and `PREFIX/share/daedalus/`. The configuration directory stays until you delete
it. Agent tools and system packages are not removed.

### Optional terminal backends

Setup shows the detected operating system and architecture. **Daedalus native
terminals** is the default: the binary supplies its own PTY/ConPTY backend and
does not need tmux or Herdr. The CLI's `existing` selection has the same no-extra-
multiplexer behavior. Select optional Herdr, tmux, or both only when you want them
included in the dependency plan. Installed tools are reused.

After setup, choose **New project → Daedalus native terminals**, or run
`daedalus shell --workspace my-project`. Detach with **Ctrl+] then D** and reopen
the terminal from the TUI. Native sessions are user-wide and survive the UI
closing. See [built-in terminals](../README.md#built-in-terminals) for discovery,
private state and platform boundaries.

On Linux, missing selected tools use the reviewed installation plan. On Windows,
setup uses installed native backends and gives installation guidance when a
selected backend is missing; it does not run Linux shell installers. Use the
Windows setup executable on native Windows. WSL is a separate Linux environment.
