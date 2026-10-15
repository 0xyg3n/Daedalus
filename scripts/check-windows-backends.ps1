#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('Herdr', 'PSMux')][string]$Backend,
    [Parameter(Mandatory = $true)][string]$BackendBinary,
    [string]$DaedalusBinary = (Join-Path $PSScriptRoot '../src/tui/target/release/daedalus-tui.exe')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Backend smoke checks require native Windows.' }
$BackendBinary = (Resolve-Path -LiteralPath $BackendBinary).Path
$DaedalusBinary = (Resolve-Path -LiteralPath $DaedalusBinary).Path
if ([IO.Path]::GetExtension($BackendBinary) -ne '.exe' -or [IO.Path]::GetExtension($DaedalusBinary) -ne '.exe') { throw 'Select native .exe files.' }
if ($Backend -eq 'Herdr' -and [IO.Path]::GetFileName($BackendBinary) -ine 'herdr.exe') { throw 'Herdr smoke requires the selected executable to be named herdr.exe.' }
function Quote-Argument([string]$Value) {
    # CRT encoding for ProcessStartInfo.Arguments; no shell interprets this text.
    $quoted = '"'; $slashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') { $slashes++; continue }
        if ($character -eq '"') { $quoted += ('\' * ($slashes * 2 + 1)) }
        else { $quoted += ('\' * $slashes) }
        $quoted += $character; $slashes = 0
    }
    $quoted + ('\' * ($slashes * 2)) + '"'
}
function Set-PrivateAccess([string]$Path, [bool]$Directory) {
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    if ($Directory) { $acl = New-Object Security.AccessControl.DirectorySecurity }
    else { $acl = New-Object Security.AccessControl.FileSecurity }
    $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true, $false)
    if ($Directory) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    } else { $rule = New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'Allow') }
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
}
function Write-Private([string]$Path, [string]$Text) {
    if (-not (Test-Path -LiteralPath $Path)) { [IO.File]::WriteAllBytes($Path, [byte[]]@()) }
    Set-PrivateAccess $Path $false
    [IO.File]::WriteAllText($Path, $Text, (New-Object Text.UTF8Encoding($false)))
}
function Start-Owned([string]$Executable, [string[]]$Arguments, [bool]$Capture = $true) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.Arguments = (($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' ')
    $info.WorkingDirectory = $project
    $info.UseShellExecute = $false; $info.CreateNoWindow = $true
    $info.EnvironmentVariables.Clear()
    foreach ($key in @('SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TEMP', 'TMP', 'PROCESSOR_ARCHITECTURE', 'NUMBER_OF_PROCESSORS')) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($value) { $info.EnvironmentVariables[$key] = $value }
    }
    foreach ($key in $fixtureEnvironment.Keys) { $info.EnvironmentVariables[$key] = $fixtureEnvironment[$key] }
    $info.RedirectStandardOutput = $Capture; $info.RedirectStandardError = $Capture
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    if (-not $process.Start()) { throw 'Owned fixture process did not start.' }
    $process
}
function Invoke-Owned([string]$Executable, [string[]]$Arguments, [switch]$AllowFailure) {
    $process = Start-Owned $Executable $Arguments
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(20000)) {
            $process.Kill(); [void]$process.WaitForExit(5000)
            throw 'Owned fixture command exceeded 20 seconds; no command is retried.'
        }
        if (-not $stdout.Wait(1000) -or -not $stderr.Wait(1000)) { throw 'Owned fixture output remained open.' }
        if ($stdout.Result.Length -gt 4000000 -or $stderr.Result.Length -gt 1000000) { throw 'Fixture output exceeded limit.' }
        $result = [pscustomobject]@{ Code = $process.ExitCode; Output = $stdout.Result; Error = $stderr.Result }
        if ($result.Code -ne 0 -and -not $AllowFailure) { throw "Fixture command failed: $Executable (exit $($result.Code)): $($result.Error)" }
        $result
    } finally { $process.Dispose() }
}
function Invoke-Daedalus([string[]]$Arguments) { (Invoke-Owned $DaedalusBinary $Arguments).Output | ConvertFrom-Json }
$id = [guid]::NewGuid().ToString('N')
$name = 'daedalus-smoke-' + $id
$root = Join-Path ([IO.Path]::GetTempPath()) $name
$project = Join-Path $root 'empty-project'
$configDirectory = Join-Path $root 'instance'
$endpoint = Join-Path $root 'mux-endpoint'
$marker = 'daedalus-smoke-' + [guid]::NewGuid().ToString('N')
$server = $null; $created = $false; $startedAt = [DateTime]::Now
$helper = Join-Path $root 'fixture-echo.exe'
[void][IO.Directory]::CreateDirectory($root); Set-PrivateAccess $root $true
[void][IO.Directory]::CreateDirectory($project); Set-PrivateAccess $project $true
$fixtureEnvironment = @{
    HOME = $root; USERPROFILE = $root; APPDATA = $root; LOCALAPPDATA = $root
    PATH = ([IO.Path]::GetDirectoryName($BackendBinary) + ';' + $env:PATH)
    HERDR_CONFIG_PATH = (Join-Path $root 'herdr.toml')
    DAEDALUS_TMUX_BIN = $BackendBinary
    TEMP = $root; TMP = $root
}
try {
    # A native echo program avoids profiles, shells, network and provider code.
    $helper = Join-Path $root 'fixture-echo.exe'
    $source = Join-Path $root 'fixture-echo.cs'
    Write-Private $source @'
using System;
using System.Text.RegularExpressions;
class FixtureEcho {
    static void Main() {
        Console.WriteLine("daedalus-fixture-ready");
        string line;
        while ((line = Console.ReadLine()) != null) {
            if (Regex.IsMatch(line, "^daedalus-smoke-[a-f0-9]{32}$")) Console.WriteLine("received:" + line);
            else if (line.Length != 0) Console.WriteLine("fixture-refused");
        }
    }
}
'@
    $compiler = @(
        (Join-Path $env:WINDIR 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'),
        (Join-Path $env:WINDIR 'Microsoft.NET/Framework/v4.0.30319/csc.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $compiler) { throw 'The installed .NET Framework C# compiler is required for the isolated native echo fixture; no compiler is installed automatically.' }
    [void](Invoke-Owned $compiler @('/nologo', '/noconfig', '/r:System.dll', '/target:exe', ('/out:' + $helper), $source))
    Set-PrivateAccess $helper $false
    [void](Invoke-Daedalus @('config', 'init', '--config-dir', $configDirectory, '--workspace', $project, '--apply'))
    $controlPath = Join-Path $configDirectory 'control.json'
    $control = Get-Content -LiteralPath $controlPath -Raw | ConvertFrom-Json
    $control.discovery.automatic = $false
    if ($Backend -eq 'PSMux') {
        $control.tmux_sockets = @($endpoint); $control.discovery.herdr_sessions = @()
        $emptyConfig = Join-Path $root 'empty-mux.conf'; Write-Private $emptyConfig ''
        # -S selects only this GUID-derived namespace, never the default server.
        $created = $true
        [void](Invoke-Owned $BackendBinary @('-f', $emptyConfig, '-S', $endpoint, 'new-session', '-d', '-s', $name, '-c', $project, '--', $helper))
    } else {
        $control.tmux_sockets = @(); $control.discovery.herdr_sessions = @($name)
        $shell = $helper.Replace('\', '\\').Replace('"', '\"')
        Write-Private $fixtureEnvironment.HERDR_CONFIG_PATH ("onboarding = false`n[terminal]`ndefault_shell = `"$shell`"`n[update]`nversion_check = false`nmanifest_check = false`n[session]`nresume_agents_on_restore = false`n")
        $server = Start-Owned $BackendBinary @('--session', $name, 'server') $false
        $created = $true
        $ready = $false; $deadline = [DateTime]::UtcNow.AddSeconds(20)
        while (-not $ready -and [DateTime]::UtcNow -lt $deadline) {
            $snapshotResult = Invoke-Owned $BackendBinary @('--session', $name, 'api', 'snapshot') -AllowFailure
            if ($snapshotResult.Code -eq 0) {
                $snapshot = $snapshotResult.Output | ConvertFrom-Json
                if ($snapshot.PSObject.Properties.Name -contains 'result') { $snapshot = $snapshot.result }
                if ($snapshot.PSObject.Properties.Name -contains 'snapshot') { $snapshot = $snapshot.snapshot }
                if (@($snapshot.panes).Count -eq 0) {
                    [void](Invoke-Owned $BackendBinary @('--session', $name, 'workspace', 'create', '--cwd', $project, '--label', $name, '--no-focus'))
                }
                $ready = $true
            } else { Start-Sleep -Milliseconds 200 }
        }
        if (-not $ready) { throw 'Owned Herdr server did not become ready.' }
    }
    Write-Private $controlPath ($control | ConvertTo-Json -Depth 20)
    $target = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(25)
    while (-not $target -and [DateTime]::UtcNow -lt $deadline) {
        $inventory = Invoke-Daedalus @('inventory', '--config-dir', $configDirectory, '--json')
        $matches = @($inventory.panes | Where-Object {
            if ($Backend -eq 'PSMux') { $_.backend -eq 'tmux' -and [IO.Path]::GetFullPath($_.socket) -eq [IO.Path]::GetFullPath($endpoint) -and $_.session -eq $name }
            else { $_.backend -eq 'herdr' -and $_.session -eq $name }
        })
        if ($matches.Count -eq 1) { $target = $matches[0] }
        elseif ($matches.Count -gt 1) { throw 'Isolated fixture returned ambiguous terminals.' }
        if (-not $target) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $target) { throw 'Isolated backend did not expose one exact terminal.' }
    $targetPath = Join-Path $root 'target.json'; Write-Private $targetPath ($target | ConvertTo-Json -Depth 20)
    $captureArgs = @('terminal', 'capture', '--config-dir', $configDirectory, '--target', $targetPath, '--lines', '80')
    $before = Invoke-Daedalus $captureArgs
    $send = @('terminal', 'send', '--config-dir', $configDirectory, '--target', $targetPath, '--text', $marker)
    $preview = Invoke-Daedalus $send
    if ($preview.apply -ne $false) { throw 'Send preview did not report inert operation.' }
    $afterPreview = Invoke-Daedalus $captureArgs
    if ($afterPreview.output -like ('*received:' + $marker + '*')) { throw 'Preview executed the input.' }
    $receipt = Invoke-Daedalus ($send + '--apply')
    if ($receipt.state -notin @('submitted', 'accepted')) { throw 'Submission was uncertain; the test will not retry.' }
    [void](Invoke-Daedalus @('terminal', 'keys', '--config-dir', $configDirectory, '--target', $targetPath, '--keys', 'Enter', '--apply'))
    $seen = $false; $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not $seen -and [DateTime]::UtcNow -lt $deadline) {
        $capture = Invoke-Daedalus $captureArgs
        $seen = $capture.output -like ('*received:' + $marker + '*')
        if (-not $seen) { Start-Sleep -Milliseconds 150 }
    }
    if (-not $seen) { throw 'No native echo response was captured.' }
    $stale = $target | ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $stale.terminal_id = $stale.terminal_id + '-stale'
    $stalePath = Join-Path $root 'stale.json'; Write-Private $stalePath ($stale | ConvertTo-Json -Depth 20)
    $rejected = Invoke-Owned $DaedalusBinary @('terminal', 'capture', '--config-dir', $configDirectory, '--target', $stalePath) -AllowFailure
    if ($rejected.Code -eq 0) { throw 'Stale terminal generation was accepted.' }
    Write-Host "$Backend isolated backend smoke passed (native echo only; no harness, provider or Telegram validation)."
} finally {
    $cleanupFailed = $false
    if ($created) {
        try {
            if ($Backend -eq 'PSMux') { [void](Invoke-Owned $BackendBinary @('-S', $endpoint, 'kill-server')) }
            else {
                [void](Invoke-Owned $BackendBinary @('session', 'stop', $name))
                [void](Invoke-Owned $BackendBinary @('session', 'delete', $name))
            }
        } catch { $cleanupFailed = $true; Write-Warning "Isolated backend cleanup needs review: $name ($($_.Exception.Message))" }
    }
    # The fixture path is GUID-owned and protected; never select helpers by PID alone.
    foreach ($process in @(Get-Process -Name 'fixture-echo' -ErrorAction SilentlyContinue)) {
        try {
            [void]$process.Handle
            if ($process.Path -eq $helper -and $process.StartTime -ge $startedAt -and -not $process.HasExited) {
                $process.Kill(); [void]$process.WaitForExit(5000)
            }
        } catch { $cleanupFailed = $true; Write-Warning 'Owned echo helper cleanup could not be confirmed.' }
        finally { $process.Dispose() }
    }
    if ($server) {
        try { if (-not $server.HasExited) { $server.Kill(); [void]$server.WaitForExit(5000) } }
        finally { $server.Dispose() }
    }
    if (-not $cleanupFailed) { Remove-Item -LiteralPath $root -Recurse -Force }
    else {
        Write-Warning "Retained only the owned fixture directory for cleanup: $root"
        throw 'Backend smoke cleanup was not confirmed; review the retained GUID fixture.'
    }
}
