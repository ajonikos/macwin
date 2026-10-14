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

function Send-SettingChange {
    # Removing a variable that does not exist changes nothing, but .NET still broadcasts
    # WM_SETTINGCHANGE, so Explorer starts new programs with the new PATH.
    [Environment]::SetEnvironmentVariable('MACWIN_SETTING_CHANGE', $null, 'User')
}

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

# Only our entry goes; the rest of the raw value and its type stay (see install.ps1).
$envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
if ($envKey) {
    try {
        $raw = [string]$envKey.GetValue('Path', '',
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $parts = @($raw -split ';' | Where-Object { $_ })
        $kept = @($parts | Where-Object { $_ -ne $Destination })
        if ($kept.Count -ne $parts.Count) {
            if ($kept.Count) {
                $envKey.SetValue('Path', ($kept -join ';'), $envKey.GetValueKind('Path'))
            } else {
                $envKey.DeleteValue('Path')
            }
            Send-SettingChange
        }
    } finally {
        $envKey.Close()
    }
}

# A folder cannot be removed while it is the current directory, which it is when this
# script was started from it (for example with "Run with PowerShell").
$neutral = [IO.Path]::GetTempPath()
Set-Location -LiteralPath $neutral
[Environment]::CurrentDirectory = $neutral
if (Test-Path -LiteralPath $Destination) {
    Remove-Item -LiteralPath $Destination -Recurse -Force
}
if ($Purge) {
    foreach ($dir in (Join-Path $env:APPDATA 'macwin'), (Join-Path $env:LOCALAPPDATA 'macwin')) {
        if (Test-Path $dir) {
            Remove-Item $dir -Recurse -Force
        }
    }
}
Write-Host 'macwin is removed.'
