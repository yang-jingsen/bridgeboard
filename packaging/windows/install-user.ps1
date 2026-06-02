param(
    [switch]$NoStartup,
    [string]$InstallBin = "$env:USERPROFILE\.cargo\bin"
)

$ErrorActionPreference = "Stop"

$packageRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$bridgeboardExe = Join-Path $packageRoot "bridgeboard.exe"
$trayExe = Join-Path $packageRoot "bridgeboard-tray.exe"
$uiExe = Join-Path $packageRoot "bridgeboard-ui.exe"
$iconFile = Join-Path $packageRoot "bridgeboard.ico"

foreach ($required in @($bridgeboardExe, $trayExe, $uiExe, $iconFile)) {
    if (-not (Test-Path $required)) {
        throw "Missing package file: $required"
    }
}

New-Item -ItemType Directory -Force $InstallBin | Out-Null
Get-Process -Name bridgeboard, bridgeboard-tray, bridgeboard-ui -ErrorAction SilentlyContinue | Stop-Process -Force

Copy-Item $bridgeboardExe (Join-Path $InstallBin "bridgeboard.exe") -Force
Copy-Item $trayExe (Join-Path $InstallBin "bridgeboard-tray.exe") -Force
Copy-Item $uiExe (Join-Path $InstallBin "bridgeboard-ui.exe") -Force
Copy-Item $iconFile (Join-Path $InstallBin "bridgeboard.ico") -Force

$appConfigDir = Join-Path $env:APPDATA "bridgeboard"
New-Item -ItemType Directory -Force $appConfigDir | Out-Null

$shareDir = Join-Path $env:APPDATA "bridgeboard\examples"
New-Item -ItemType Directory -Force $shareDir | Out-Null
if (Test-Path (Join-Path $packageRoot "examples")) {
    Copy-Item (Join-Path $packageRoot "examples\*") $shareDir -Force
}

if (-not $NoStartup) {
    $startup = [Environment]::GetFolderPath("Startup")
    $shortcutPath = Join-Path $startup "Bridgeboard Tray.lnk"
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = Join-Path $InstallBin "bridgeboard-tray.exe"
    $shortcut.Arguments = ""
    $shortcut.WorkingDirectory = $InstallBin
    $shortcut.IconLocation = Join-Path $InstallBin "bridgeboard.ico"
    $shortcut.Save()
}

& (Join-Path $InstallBin "bridgeboard.exe") registry export --json
