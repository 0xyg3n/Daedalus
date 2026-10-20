# Installation

Daedalus runs on Linux and Windows as one native executable. It includes the TUI,
CLI, Telegram service and hooks. The project is in active development; release
packages target Linux x86-64 and Windows x64. Agent tools keep their own runtimes
and accounts.

## Linux install

Prebuilt x86-64 executables and matching `.sha256` files are available on
[Releases](https://github.com/0xyg3n/Daedalus/releases). Verify the checksum with
`sha256sum -c` and use the installer's `--binary` and `--sha256` options for a
managed installation. To build from source:

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
it from a source you trust. The Linux source installer also supports ARM64.

## Native Windows install

Download `Daedalus-<version>-setup.exe` from [Releases](https://github.com/0xyg3n/Daedalus/releases)
and double-click it. The per-user installer needs no PowerShell script, Git or
compiler. It installs under `%LOCALAPPDATA%\Daedalus`, creates a Start Menu shortcut
and opens setup. The release also includes a standalone executable and SHA-256
checksums. Install a standalone executable before using in-app updates.

The built-in **Daedalus native terminals** backend uses ConPTY; Herdr is optional.
Native Windows multiplexers such as [tmux-windows](https://github.com/arndawg/tmux-windows)
and [PSMux](https://github.com/psmux/psmux) can be selected through explicitly
configured endpoints. Unix socket discovery is not used on Windows. WSL
sessions are a separate environment. Set `DAEDALUS_TMUX_BIN` to the absolute
path of a compatible executable when selection is needed; discovery otherwise
tries `tmux.exe`, then `psmux.exe`. PSMux `.psmux/*.port` namespace markers are
automatic candidates; configured absolute `tmux_sockets` endpoints are passed
through `-S` for the selected implementation. Every candidate still needs fresh
identity evidence before mutation. Install native Windows versions of Herdr and
any harnesses you choose; Linux shell installers are not run on Windows. Native
harness launch requires an `.exe`; `.cmd` and `.bat` wrappers are refused.

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

Windows packages target x64. On ARM64, `-AllowEmulation` explicitly selects x64
emulation; there is no native Windows ARM64 package.
Installation never starts a bot, service, private setup or provider sign-in.
Guided first launch with hooks is unavailable on Windows. Existing verified
Herdr processes have native hook contracts for Claude Code 2.1.296+, Hermes, Pi
and OpenCode. Codex/Grok/Cursor hook enrollment and hooks through native
tmux-compatible backends are not enabled on Windows. Herdr duplicate attachment views
are unavailable; use Watch. Remote feeding still requires exact process,
terminal generation and conversation registration. See the [Windows
support table](HARNESSES.md#native-windows-boundaries) for available operations.

### Windows local validation

```powershell
.\scripts\check-windows.ps1
```

This runs formatting, tests, Clippy, a release build and isolated installer
fixtures. The optional `-BackendSmoke Herdr|PSMux -BackendBinary ABS` checks an
installed backend using a private echo fixture. See [testing](ARCHITECTURE.md#testing)
for the test suites and contributor workflow.

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
The setup menu also offers **Install standard hooks** after an agent is running.
It discovers the live sessions and guides each verified native profile through
its own review; leaving that step for later does not block terminal control.
For a bot connection, follow the [Telegram guide](TELEGRAM.md): review the token
and owner ID, register a DM or group, select a destination, activate its hooks and
start the connection. Existing configuration can be edited from Settings.

## Configuration directory

Daedalus uses the first of: `--config-dir`, `DAEDALUS_CONFIG_DIR`,
`$XDG_CONFIG_HOME/daedalus`, then the platform default (`%APPDATA%\Daedalus`
on Windows, `~/.config/daedalus` on Linux).
If `DAEDALUS_CONFIG_DIR` is relative, Daedalus resolves it under your user
profile so changing the launch directory does not switch configurations.

Create configuration with `daedalus setup` or
`daedalus config init --config-dir ABS --workspace ABS --apply`.
[control.example.json](../config/control.example.json) documents the public
`control.json` schema. Replace its placeholder paths, then validate with
`daedalus config validate --from FILE`.

| File | Purpose |
| --- | --- |
| `control.json` | Workspace, harness selections, profile paths and terminal discovery endpoints |
| `telegram-service.json` | Bot ID, token file, sender grants and feeder/reply bindings |
| `hindsight.json` | Optional memory endpoint, credential file and bank per registration |
| `gateway.json` | Optional Claude Code or Codex endpoint override |
| `armonia.sqlite3` | Optional existing Armonia identity registry |

The same directory holds routes, deliveries, checkpoints, layouts, receipts and
attachments. Keep this directory outside Git. Subscription labels are not
credentials; each harness manages its own account.

The native terminal registry is shared by your local user across configuration
instances: `~/.local/share/daedalus/terminals` on Linux and
`%LOCALAPPDATA%\Daedalus\terminals` on Windows. Keep it outside Git as well.

To copy an existing instance into a new directory, run
`daedalus config import-instance --config-dir NEW --from OLD` (preview; `--apply`
writes, `--credentials` also copies token files). If the old instance ran the
Telegram service, stop it and hand over with `daedalus telegram cutover preview|apply`.

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
On Windows, approving an update in the TUI verifies the setup package, closes
Daedalus and runs that installer. The installer waits for the old process to exit,
then replaces the current-user installation and opens Daedalus. The command-line
`update install` returns the verified installer path for manual launch after
closing Daedalus. Neither path restarts Telegram or changes private configuration.
Stop the managed Telegram service from Telegram settings before approving a Windows
update, and close other Daedalus instances: Windows cannot replace an executable
while another process is using it. Start Telegram again after installation.

On Windows, use **Installed apps → Daedalus → Uninstall**. The uninstaller verifies
the installation receipt, requests a graceful shutdown of the managed Telegram
service in the default and saved instances, and waits for the process and its
ownership lock to be released. It never force-kills agents or terminals. Close
other Daedalus windows first. If shutdown times out or process identity cannot
be verified, the installation stays in place so you can retry. Private
configuration, Telegram routes and credentials are preserved. Optional harnesses
and terminal backends are not removed.

On Linux, stop its Telegram service, then delete `PREFIX/bin/daedalus`
and `PREFIX/share/daedalus/`. The configuration directory stays until you delete
it. Agent tools and system packages are not removed.
