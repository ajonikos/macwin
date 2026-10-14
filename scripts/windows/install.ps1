<#
.SYNOPSIS
Installs macwin for the current user.

.DESCRIPTION
Copies macwin.exe and macwin-tray.exe to %LOCALAPPDATA%\Programs\macwin, adds
that folder to the user's PATH, starts the tray at login (`macwin autostart
on`), allows macwin through Windows Firewall (UDP 47800 for sessions and
pairing, 47801 for discovery; Windows asks for administrator rights once) and
starts the tray. Running it again updates an existing installation.
#>
[CmdletBinding()]
param(
    # Install from this folder instead of the script's own.
    [string]$Source = $PSScriptRoot,
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\macwin'),
    # Also allow macwin on networks marked Public.
    [switch]$AllNetworks,
    [switch]$NoFirewall,
    [switch]$NoAutostart,
    [switch]$NoPath,
    [switch]$NoStart
)
$ErrorActionPreference = 'Stop'

$programs = 'macwin.exe', 'macwin-tray.exe'
foreach ($exe in $programs) {
    if (-not (Test-Path (Join-Path $Source $exe))) {
        throw "$exe is missing in $Source"
    }
}

function Stop-Macwin([string]$Folder) {
    # A running copy keeps its files locked; its peer just sees a disconnect.
    Get-Process -Name 'macwin', 'macwin-tray' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Folder, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force -PassThru |
        Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

Stop-Macwin $Destination
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
foreach ($file in $programs + 'uninstall.ps1', 'README.txt', 'THIRD_PARTY_NOTICES') {
    $path = Join-Path $Source $file
    if (Test-Path $path) {
        Copy-Item $path $Destination -Force
    }
}
Write-Host "Installed in $Destination"

if (-not $NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    if ($parts -notcontains $Destination) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $Destination) -join ';'), 'User')
        Write-Host 'Added to PATH: new terminals know `macwin`.'
    }
}

if (-not $NoAutostart) {
    & (Join-Path $Destination 'macwin.exe') autostart on
    if ($LASTEXITCODE -ne 0) {
        throw '`macwin autostart on` failed'
    }
}

if (-not $NoFirewall) {
    $profiles = if ($AllNetworks) { 'Private,Domain,Public' } else { 'Private,Domain' }
    $folder = $Destination -replace "'", "''"
    $rules = @"
Remove-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue
foreach (`$exe in 'macwin.exe', 'macwin-tray.exe') {
    New-NetFirewallRule -DisplayName "macwin (`$exe)" -Group 'macwin' -Direction Inbound ``
        -Action Allow -Protocol UDP -LocalPort 47800,47801 -Profile $profiles ``
        -Program (Join-Path '$folder' `$exe) | Out-Null
}
"@
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    try {
        if ($admin) {
            Invoke-Expression $rules
        } else {
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($rules))
            $p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden `
                -ArgumentList '-NoProfile', '-EncodedCommand', $encoded
            if ($p.ExitCode -ne 0) {
                throw "exit code $($p.ExitCode)"
            }
        }
        Write-Host "Allowed through Windows Firewall ($profiles networks)."
    } catch {
        Write-Warning "The firewall rules were not added ($_). Windows asks when macwin first listens."
    }
}

if (-not $NoStart) {
    Start-Process (Join-Path $Destination 'macwin-tray.exe')
    Write-Host 'macwin is running: its icon is in the notification area.'
}
