#requires -Version 5.1
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$Source,
    [string]$Ref = 'HEAD',
    [string]$Binary,
    [uri]$DownloadUrl,
    [string]$Sha256,
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Daedalus'),
    [switch]$Plan,
    [switch]$InstallDependencies,
    [switch]$AllowEmulation
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command failed (exit $LASTEXITCODE)." }
}
function Get-Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Assert-WindowsExecutable([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $reader = New-Object IO.BinaryReader($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5a4d) { throw 'Not a Windows PE executable.' }
        $stream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        if ($offset -gt $stream.Length - 26) { throw 'Invalid PE header offset.' }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw 'Expected an x64 Windows executable.'
        }
        $stream.Position = $offset + 24
        if ($reader.ReadUInt16() -ne 0x020b) { throw 'Expected a PE32+ executable.' }
    } finally { $reader.Dispose(); $stream.Dispose() }
}
if ($env:OS -ne 'Windows_NT') { throw 'Run this installer in native Windows PowerShell.' }
$architecture = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $architecture = $env:PROCESSOR_ARCHITEW6432 }
if ($architecture -eq 'ARM64') {
    if (-not $AllowEmulation) { throw 'Native ARM64 releases are not provided. x64 emulation requires -AllowEmulation and remains unverified.' }
    Write-Warning 'Installing x64 for Windows ARM64 emulation; this path is not hardware-validated.'
} elseif ($architecture -ne 'AMD64') { throw "Unsupported Windows architecture: $architecture" }
$modeCount = [int][bool]$Source + [int][bool]$Binary + [int][bool]$DownloadUrl
if ($modeCount -ne 1) { throw 'Choose exactly one: -Source CHECKOUT, -Binary EXE, or -DownloadUrl HTTPS_URL.' }
if (($Binary -or $DownloadUrl) -and $Sha256 -notmatch '^[a-fA-F0-9]{64}$') {
    throw 'Prebuilt installation requires -Sha256 with a trusted expected SHA-256.'
}
if ($Source -and $Sha256) { throw '-Sha256 applies only to a prebuilt executable.' }
if ($Ref -match '^[\s-]' -or $Ref -match '[\x00-\x1f\x7f]' -or [string]::IsNullOrWhiteSpace($Ref)) { throw 'Ref must be a nonempty Git revision, not an option.' }
if ($DownloadUrl -and ($DownloadUrl.Scheme -ne 'https' -or $DownloadUrl.UserInfo)) {
    throw 'DownloadUrl must be HTTPS without embedded credentials.'
}
if (-not [IO.Path]::IsPathRooted($Prefix)) { throw 'Prefix must be an absolute path.' }
$Prefix = [IO.Path]::GetFullPath($Prefix)
$binDirectory = Join-Path $Prefix 'bin'
$destination = Join-Path $binDirectory 'daedalus.exe'
$receiptPath = Join-Path $Prefix 'install.json'
if (Test-Path -LiteralPath $destination) {
    if (-not (Test-Path -LiteralPath $receiptPath)) { throw 'Refusing to replace an executable not owned by this installer.' }
    $prior = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
    if ($prior.version -ne 1 -or $prior.sha256 -notmatch '^[a-f0-9]{64}$' -or (Get-Hash $destination) -ne $prior.sha256) {
        throw 'Installed executable changed or ownership receipt is invalid; review it before replacement.'
    }
}
$missing = @()
if ($Source) {
    $Source = (Resolve-Path -LiteralPath $Source).Path
    if (-not (Test-Path -LiteralPath (Join-Path $Source 'src/tui/Cargo.toml'))) { throw 'Source is not a Daedalus checkout.' }
    foreach ($tool in @('git', 'cargo')) {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { $missing += $tool }
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $hasLinker = [bool](Get-Command link.exe -ErrorAction SilentlyContinue)
    if (-not $hasLinker -and (Test-Path -LiteralPath $vswhere)) {
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $hasLinker = [bool]$vs
    }
    if (-not $hasLinker) { $missing += 'msvc' }
} else {
    if ($Binary) {
        $Binary = (Resolve-Path -LiteralPath $Binary).Path
        if ((Get-Hash $Binary) -ne $Sha256.ToLowerInvariant()) { throw 'Binary checksum mismatch.' }
        Assert-WindowsExecutable $Binary
    }
}
Write-Host "Target: x86_64-pc-windows-msvc; prefix: $Prefix"
if ($Source) { Write-Host "Build committed revision '$Ref' from $Source in a temporary checkout (working edits excluded)." }
if ($Binary) { Write-Host "Install reviewed local executable: $Binary" }
if ($DownloadUrl) { Write-Host "Download executable over HTTPS; verify SHA-256 before installation." }
Write-Host 'Install versioned executable and bin/daedalus.exe; preserve private configuration. No PATH, bot, service or profile changes.'
if ($missing.Count) {
    Write-Host ('Missing build prerequisites: ' + ($missing -join ', '))
    if ($missing -contains 'git') { Write-Host 'winget install --exact --id Git.Git' }
    if ($missing -contains 'cargo') { Write-Host 'winget install --exact --id Rustlang.Rustup' }
    if ($missing -contains 'msvc') { Write-Host 'winget install --exact --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"' }
}
if ($Plan -or $WhatIfPreference) { return }
if ($missing.Count) {
    if (-not $InstallDependencies) { throw 'Install the listed prerequisites, reopen PowerShell, then retry; or explicitly select -InstallDependencies.' }
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) { throw 'winget is unavailable; install the displayed prerequisites manually.' }
    foreach ($tool in $missing) {
        $package = switch ($tool) { 'git' { 'Git.Git' } 'cargo' { 'Rustlang.Rustup' } 'msvc' { 'Microsoft.VisualStudio.2022.BuildTools' } }
        if ($PSCmdlet.ShouldProcess($package, 'Install missing build prerequisite using winget')) {
            $installArgs = @('install', '--exact', '--id', $package)
            if ($tool -eq 'msvc') { $installArgs += @('--override', '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended') }
            Invoke-Checked 'winget' $installArgs
        }
    }
    throw 'Dependency installation finished. Reopen PowerShell so PATH/toolchains refresh, then rerun without -InstallDependencies.'
}
if (-not $PSCmdlet.ShouldProcess($destination, 'Build/download, verify and install Daedalus')) { return }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('daedalus-install-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($temporary)
try {
    $candidate = $Binary
    if ($DownloadUrl) {
        $candidate = Join-Path $temporary 'download.exe'
        # Disable redirects: the reviewed HTTPS endpoint must serve the bytes itself.
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $candidate -UseBasicParsing -MaximumRedirection 0
    }
    if ($Source) {
        $checkout = Join-Path $temporary 'source'
        Invoke-Checked 'git' @('clone', '--no-checkout', '--no-hardlinks', '--', $Source, $checkout)
        Invoke-Checked 'git' @('-C', $checkout, 'checkout', '--detach', $Ref, '--')
        $manifest = Join-Path $checkout 'src/tui/Cargo.toml'
        $buildDirectory = Join-Path $temporary 'build'
        Invoke-Checked 'cargo' @('build', '--manifest-path', $manifest, '--target-dir', $buildDirectory, '--release', '--locked', '--target', 'x86_64-pc-windows-msvc')
        $candidate = Join-Path $buildDirectory 'x86_64-pc-windows-msvc/release/daedalus-tui.exe'
    }
    Assert-WindowsExecutable $candidate
    $hash = Get-Hash $candidate
    if (-not $Source -and $hash -ne $Sha256.ToLowerInvariant()) { throw 'Downloaded/copied executable checksum mismatch.' }
    $release = Join-Path $Prefix ('share/daedalus/native/' + $hash)
    [void][IO.Directory]::CreateDirectory($release)
    [void][IO.Directory]::CreateDirectory($binDirectory)
    $releaseBinary = Join-Path $release 'daedalus.exe'
    if (Test-Path -LiteralPath $releaseBinary) {
        if ((Get-Hash $releaseBinary) -ne $hash) { throw 'Existing versioned executable has changed.' }
    } else { Copy-Item -LiteralPath $candidate -Destination $releaseBinary }
    $staged = Join-Path $binDirectory ('daedalus-' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        Copy-Item -LiteralPath $releaseBinary -Destination $staged
        if ((Get-Hash $staged) -ne $hash) { throw 'Installed copy failed checksum verification.' }
        Move-Item -LiteralPath $staged -Destination $destination -Force
    } finally { if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged -Force } }
    @{ version = 1; sha256 = $hash; target = 'x86_64-pc-windows-msvc' } | ConvertTo-Json | Set-Content -LiteralPath $receiptPath -Encoding UTF8
    Write-Host "Installed $destination"
    Write-Host "Run: & '$destination' setup"
} finally { Remove-Item -LiteralPath $temporary -Recurse -Force }
