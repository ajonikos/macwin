<#
.SYNOPSIS
Removes macwin for the current user.

.DESCRIPTION
Stops macwin and removes start at login, the firewall rules (Windows asks for
administrator rights once), the PATH entry and the program folder. With
-Purge it also deletes the settings, this PC's identity key and the received
files, so the computers have to pair again.
#>
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\macwin'),
    [switch]$Purge
)
$ErrorActionPreference = 'Stop'

Get-Process -Name 'macwin', 'macwin-tray' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($Destination, [StringComparison]::OrdinalIgnoreCase) } |
    Stop-Process -Force -PassThru |
    Wait-Process -Timeout 10 -ErrorAction SilentlyContinue

Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'macwin' `
    -ErrorAction SilentlyContinue

if (Get-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue) {
    $rules = "Remove-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue"
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    try {
        if ($admin) {
            Invoke-Expression $rules
        } else {
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($rules))
            Start-Process powershell.exe -Verb RunAs -Wait -WindowStyle Hidden `
                -ArgumentList '-NoProfile', '-EncodedCommand', $encoded
        }
    } catch {
        Write-Warning "The firewall rules were not removed ($_)."
    }
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @($userPath -split ';' | Where-Object { $_ -and $_ -ne $Destination })
[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')

if (Test-Path $Destination) {
    Remove-Item $Destination -Recurse -Force
}
if ($Purge) {
    foreach ($dir in (Join-Path $env:APPDATA 'macwin'), (Join-Path $env:LOCALAPPDATA 'macwin')) {
        if (Test-Path $dir) {
            Remove-Item $dir -Recurse -Force
        }
    }
}
Write-Host 'macwin is removed.'
