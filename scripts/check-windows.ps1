#requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$SkipInstallerFixtures,
    [ValidateSet('Herdr', 'PSMux')][string]$BackendSmoke,
    [string]$BackendBinary
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Run this check in native Windows PowerShell.' }
if ($BackendSmoke -and -not $BackendBinary) { throw '-BackendSmoke requires -BackendBinary with an absolute native executable path.' }
$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'src/tui/Cargo.toml'
function Invoke-Cargo([string[]]$Arguments) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $($Arguments -join ' ') failed." }
}
# Parse both scripts before any build, independent of optional lint modules.
foreach ($script in @((Join-Path $root 'install.ps1'), (Join-Path $root 'scripts/check-windows-backends.ps1'), $PSCommandPath)) {
    $tokens = $null; $parseErrors = $null
    [void][Management.Automation.Language.Parser]::ParseFile($script, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
}
Invoke-Cargo @('fmt', '--manifest-path', $manifest, '--check')
Invoke-Cargo @('test', '--manifest-path', $manifest, '--locked', '--all-targets')
Invoke-Cargo @('clippy', '--manifest-path', $manifest, '--locked', '--all-targets', '--', '-D', 'warnings')
Invoke-Cargo @('build', '--manifest-path', $manifest, '--locked', '--release')
if (-not $SkipInstallerFixtures) {
    $fixture = Join-Path ([IO.Path]::GetTempPath()) ('daedalus-installer-test-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($fixture)
    try {
        $binary = Join-Path $fixture 'fixture.exe'
        # Header-only fixture: installer verifies bytes/architecture, never executes it.
        # This prefix is not %LOCALAPPDATA%\Daedalus, so uninstall --prepare is not run.
        $bytes = New-Object byte[] 256
        $bytes[0] = 0x4d; $bytes[1] = 0x5a; $bytes[0x3c] = 0x80
        $bytes[0x80] = 0x50; $bytes[0x81] = 0x45
        $bytes[0x84] = 0x64; $bytes[0x85] = 0x86
        $bytes[0x98] = 0x0b; $bytes[0x99] = 0x02
        [IO.File]::WriteAllBytes($binary, $bytes)
        $hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        $prefix = Join-Path $fixture 'install'
        $installer = Join-Path $root 'install.ps1'
        & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -Plan
        if (Test-Path -LiteralPath $prefix) { throw 'Plan wrote installation files.' }
        & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -WhatIf
        if (Test-Path -LiteralPath $prefix) { throw 'WhatIf wrote installation files.' }
        & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -Confirm:$false
        $installed = Join-Path $prefix 'bin/daedalus.exe'
        if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $hash) { throw 'Installer copy mismatch.' }
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
        foreach ($path in @($prefix, (Split-Path $installed), $installed, (Join-Path $prefix 'install.json'))) {
            $acl = Get-Acl -LiteralPath $path
            if (-not $acl.GetOwner([Security.Principal.SecurityIdentifier]).Equals($sid) -or -not $acl.AreAccessRulesProtected) {
                throw 'Installer did not preserve private individual ownership.'
            }
        }
        $receipt = [IO.File]::ReadAllBytes((Join-Path $prefix 'install.json'))
        if ($receipt[0] -ne [byte][char]'{') { throw 'Installer receipt must be UTF-8 JSON without a BOM.' }
        # A repeat install exercises receipt validation and private atomic replacement.
        & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -Confirm:$false
        # Share the file for readers so receipt hashing works, but not for replacement.
        $locked = [IO.File]::Open($installed, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            $rejected = $false
            try { & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -Confirm:$false } catch { $rejected = $true }
            if (-not $rejected) { throw 'Installer replaced an in-use executable.' }
            if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $hash) { throw 'In-use executable was removed or changed.' }
        } finally { $locked.Dispose() }
        $rejected = $false
        try { & $installer -Binary $binary -Sha256 ('0' * 64) -Prefix $prefix -Confirm:$false } catch { $rejected = $true }
        if (-not $rejected) { throw 'Installer accepted a mismatched checksum.' }
        [IO.File]::AppendAllText($installed, 'changed')
        $rejected = $false
        try { & $installer -Binary $binary -Sha256 $hash -Prefix $prefix -Confirm:$false } catch { $rejected = $true }
        if (-not $rejected) { throw 'Installer replaced changed installed bytes.' }
    } finally { Remove-Item -LiteralPath $fixture -Recurse -Force }
}
if ($BackendSmoke) {
    & (Join-Path $root 'scripts/check-windows-backends.ps1') -Backend $BackendSmoke -BackendBinary $BackendBinary
}
Write-Host 'Native Windows local checks passed. Live provider, Telegram and hardware behavior were not tested.'
