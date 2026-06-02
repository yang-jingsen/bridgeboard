param(
    [string]$BridgeboardBin = $(if ($env:BRIDGEBOARD_BIN) { $env:BRIDGEBOARD_BIN } else { "bridgeboard" })
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$script:Services = @()
$script:Tray = New-Object System.Windows.Forms.NotifyIcon
$script:Tray.Icon = [System.Drawing.SystemIcons]::Application
$script:Tray.Text = "Bridgeboard"
$script:Tray.Visible = $true

function Invoke-Bridgeboard {
    param([string[]]$Arguments)
    $output = & $BridgeboardBin @Arguments 2>&1
    [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Text = ($output | Out-String).Trim()
    }
}

function Show-BridgeBalloon {
    param(
        [string]$Title,
        [string]$Message,
        [System.Windows.Forms.ToolTipIcon]$Icon = [System.Windows.Forms.ToolTipIcon]::Info
    )
    if ($Message.Length -gt 240) {
        $Message = $Message.Substring(0, 237) + "..."
    }
    $script:Tray.ShowBalloonTip(5000, $Title, $Message, $Icon)
}

function Start-BridgeTerminal {
    param([string[]]$Arguments)
    $quoted = @($BridgeboardBin) + $Arguments | ForEach-Object {
        if ($_ -match '\s') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
    }
    Start-Process powershell -ArgumentList @(
        "-NoExit",
        "-Command",
        ($quoted -join " ")
    )
}

function Add-Action {
    param(
        [System.Windows.Forms.MenuItem]$Parent,
        [string]$Label,
        [string[]]$Arguments
    )
    $item = New-Object System.Windows.Forms.MenuItem($Label)
    $item.add_Click({
        $result = Invoke-Bridgeboard -Arguments $Arguments
        $icon = if ($result.ExitCode -eq 0) {
            [System.Windows.Forms.ToolTipIcon]::Info
        } else {
            [System.Windows.Forms.ToolTipIcon]::Warning
        }
        Show-BridgeBalloon -Title "Bridgeboard: $Label" -Message $result.Text -Icon $icon
        Refresh-BridgeMenu
    }.GetNewClosure())
    [void]$Parent.MenuItems.Add($item)
}

function Refresh-BridgeMenu {
    $menu = New-Object System.Windows.Forms.ContextMenu
    $result = Invoke-Bridgeboard -Arguments @("ports", "--json", "--peers")
    if ($result.ExitCode -eq 0) {
        try {
            $script:Services = @($result.Text | ConvertFrom-Json)
        } catch {
            $script:Services = @()
            $result = [pscustomobject]@{ ExitCode = 1; Text = $_.Exception.Message }
        }
    }

    $summary = if ($result.ExitCode -eq 0) {
        "$($script:Services.Count) service(s)"
    } else {
        "Bridgeboard: $($result.Text)"
    }
    $header = New-Object System.Windows.Forms.MenuItem($summary)
    $header.Enabled = $false
    [void]$menu.MenuItems.Add($header)
    [void]$menu.MenuItems.Add("-")

    $refresh = New-Object System.Windows.Forms.MenuItem("Refresh")
    $refresh.add_Click({ Refresh-BridgeMenu })
    [void]$menu.MenuItems.Add($refresh)
    [void]$menu.MenuItems.Add("-")

    foreach ($service in $script:Services) {
        $serviceId = [string]$service.id
        $port = [string]$service.port
        $mode = [string]$service.service_mode
        $status = [string]$service.runtime_status
        $item = New-Object System.Windows.Forms.MenuItem("$serviceId :$port [$mode/$status]")
        Add-Action -Parent $item -Label "Open" -Arguments @("open", $serviceId)
        Add-Action -Parent $item -Label "Up" -Arguments @("up", $serviceId)
        Add-Action -Parent $item -Label "Down" -Arguments @("down", $serviceId)
        Add-Action -Parent $item -Label "Restart" -Arguments @("restart", $serviceId)
        $logs = New-Object System.Windows.Forms.MenuItem("Logs")
        $logs.add_Click({ Start-BridgeTerminal -Arguments @("logs", $serviceId, "--lines", "160") }.GetNewClosure())
        [void]$item.MenuItems.Add($logs)
        [void]$menu.MenuItems.Add($item)
    }

    [void]$menu.MenuItems.Add("-")
    $doctor = New-Object System.Windows.Forms.MenuItem("Doctor")
    $doctor.add_Click({ Start-BridgeTerminal -Arguments @("doctor") })
    [void]$menu.MenuItems.Add($doctor)
    $ports = New-Object System.Windows.Forms.MenuItem("Ports")
    $ports.add_Click({ Start-BridgeTerminal -Arguments @("ports", "--peers") })
    [void]$menu.MenuItems.Add($ports)
    [void]$menu.MenuItems.Add("-")
    $quit = New-Object System.Windows.Forms.MenuItem("Quit")
    $quit.add_Click({
        $script:Tray.Visible = $false
        [System.Windows.Forms.Application]::Exit()
    })
    [void]$menu.MenuItems.Add($quit)

    $script:Tray.ContextMenu = $menu
    $script:Tray.Text = "Bridgeboard - $summary"
}

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 5000
$timer.add_Tick({ Refresh-BridgeMenu })
$timer.Start()

Refresh-BridgeMenu
[System.Windows.Forms.Application]::Run()
