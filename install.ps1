# ==============================================================================
#  ZTE K12 Mobile Controller & IP Rotator - 1-Click Windows Installer
#  (100% Pure PowerShell Edition - No VBScript Dependencies)
# ==============================================================================

$ErrorActionPreference = "Stop"

Write-Host ""
Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host "   🚀 ZTE K12 Mobile Controller & IP Rotator - Windows Installer   " -ForegroundColor Yellow
Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host ""

$InstallDir = "$env:LOCALAPPDATA\zte-k12-rotator"
$ZipUrl = "https://raw.githubusercontent.com/miwaniza/zte-k12-rotator/main/dist/zte-k12-rotator-windows.zip"
$TempZip = "$env:TEMP\zte-k12-rotator-windows.zip"

Write-Host "[*] Target Installation Directory: $InstallDir" -ForegroundColor Gray

# 1. Create target directory
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

# 2. Download package
Write-Host "[*] Downloading latest release from GitHub..." -ForegroundColor Cyan
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
    Invoke-WebRequest -Uri $ZipUrl -OutFile $TempZip -UseBasicParsing
} catch {
    Write-Host "[-] Failed to download package from GitHub: $_" -ForegroundColor Red
    Exit 1
}

# 3. Extract package
Write-Host "[*] Extracting application files..." -ForegroundColor Cyan
try {
    Expand-Archive -Path $TempZip -DestinationPath $InstallDir -Force
    Remove-Item -Path $TempZip -Force -ErrorAction SilentlyContinue
} catch {
    Write-Host "[!] Extraction notice: $_" -ForegroundColor Yellow
}

# 4. Write pure PowerShell rotation script
$PsRotateContent = @'
$ErrorActionPreference = "SilentlyContinue"
$InstallDir = "$env:LOCALAPPDATA\zte-k12-rotator"
if (-not (Test-Path "$InstallDir\zte-control.exe")) { $InstallDir = $PSScriptRoot }

# 1. Trigger rotation
try {
    $resp = Invoke-RestMethod -Uri "http://127.0.0.1:8080/api/reconnect" -TimeoutSec 6
} catch {
    Start-Process "$InstallDir\zte-control.exe" -ArgumentList "reconnect" -Wait -WindowStyle Hidden
}

Start-Sleep -Seconds 3

# 2. Query status & new public IP
$NewIp = "--"
$City = "Київ"
$Country = "UA"
$Isp = "Kyivstar"
$Band = "LTE"
$Pci = "--"
$Rsrp = "--"

try {
    $geo = Invoke-RestMethod -Uri "https://ipwho.is/" -TimeoutSec 4
    if ($geo.ip) {
        $NewIp = $geo.ip
        if ($geo.city) { $City = $geo.city }
        if ($geo.connection.isp) { $Isp = $geo.connection.isp }
    }
} catch {}

try {
    $st = Invoke-RestMethod -Uri "http://127.0.0.1:8080/goform/goform_get_cmd_process?cmd=wan_active_band,lte_pci,lte_rsrp,wan_ipaddr&multi_data=1&isTest=false" -TimeoutSec 3
    if ($st.wan_active_band) { $Band = $st.wan_active_band }
    if ($st.lte_pci) { $Pci = $st.lte_pci }
    if ($st.lte_rsrp) { $Rsrp = $st.lte_rsrp }
} catch {}

# 3. Show Native Windows Toast Notification
$toastShown = $false
try {
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null

    $template = @"
<toast duration="short">
    <visual>
        <binding template="ToastGeneric">
            <text>📡 ZTE K12: IP успішно змінено! ✅</text>
            <text>🌐 Новий IP: $NewIp ($City, $Isp)</text>
            <text>🗼 Вишка: $Band (PCI $Pci) | RSRP: $Rsrp dBm</text>
        </binding>
    </visual>
</toast>
"@
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml($template)
    $toast = New-Object Windows.UI.Notifications.ToastNotification $xml
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier("ZTE K12 IP Rotator").Show($toast)
    $toastShown = $true
} catch {}

if (-not $toastShown) {
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $notify = New-Object System.Windows.Forms.NotifyIcon
        $notify.Icon = [System.Drawing.SystemIcons]::Information
        $notify.BalloonTipTitle = "📡 ZTE K12: Новий IP отримано! ✅"
        $notify.BalloonTipText = "IP: $NewIp ($City, $Isp) | $Band (PCI $Pci)"
        $notify.Visible = $true
        $notify.ShowBalloonTip(4000)
        Start-Sleep -Seconds 4
        $notify.Dispose()
    } catch {}
}
'@
Set-Content -Path "$InstallDir\rotate-and-notify.ps1" -Value $PsRotateContent -Force

# 5. Add to User PATH
$UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -notlike "*$InstallDir*") {
    Write-Host "[+] Adding $InstallDir to User PATH..." -ForegroundColor Green
    [Environment]::SetEnvironmentVariable("PATH", "$UserPath;$InstallDir", "User")
    $env:PATH += ";$InstallDir"
}

# 6. Register Background Service via PowerShell
Write-Host "[*] Configuring Background Service..." -ForegroundColor Cyan
try {
    $TaskName = "ZTEK12RotatorService"
    $TaskAction = "powershell.exe -ExecutionPolicy Bypass -NoProfile -WindowStyle Hidden -Command `"Start-Process '$InstallDir\zte-control.exe' -ArgumentList 'ui','--no-open' -WindowStyle Hidden`""

    & schtasks /Create /TN $TaskName /TR $TaskAction /SC ONLOGON /RL HIGHEST /F 2>$null
    if ($LASTEXITCODE -ne 0) {
        & schtasks /Create /TN $TaskName /TR $TaskAction /SC ONLOGON /F 2>$null
    }
    & schtasks /Run /TN $TaskName 2>$null
    Write-Host "[+] Background Service registered & started!" -ForegroundColor Green
} catch {
    Write-Host "[!] Service notice: $_" -ForegroundColor Yellow
}

# 7. Create Pure PowerShell Shortcuts (Desktop, Start Menu, Taskbar)
try {
    $WshShell = New-Object -ComObject WScript.Shell
    $DesktopPath = [Environment]::GetFolderPath("Desktop")
    $StartMenuPath = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
    $TaskBarPath = "$env:APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"

    # Shortcut 1: Open Dashboard
    $ScDash = $WshShell.CreateShortcut("$DesktopPath\ZTE K12 Controller.lnk")
    $ScDash.TargetPath = "$InstallDir\start-dashboard.bat"
    $ScDash.WorkingDirectory = $InstallDir
    $ScDash.Description = "ZTE K12 Web Controller Dashboard"
    $ScDash.Save()

    $ScDashSm = $WshShell.CreateShortcut("$StartMenuPath\ZTE K12 Controller.lnk")
    $ScDashSm.TargetPath = "$InstallDir\start-dashboard.bat"
    $ScDashSm.WorkingDirectory = $InstallDir
    $ScDashSm.Description = "ZTE K12 Web Controller Dashboard"
    $ScDashSm.Save()

    # Shortcut 2: 1-Click Fast Rotation with Pure PowerShell
    $PsArgs = "-ExecutionPolicy Bypass -NoProfile -WindowStyle Hidden -File `"$InstallDir\rotate-and-notify.ps1`""
    
    $ScRotate = $WshShell.CreateShortcut("$DesktopPath\⚡ Ротація IP (ZTE).lnk")
    $ScRotate.TargetPath = "powershell.exe"
    $ScRotate.Arguments = $PsArgs
    $ScRotate.WorkingDirectory = $InstallDir
    $ScRotate.Description = "1-Click IP Rotation with Toast Notification"
    $ScRotate.IconLocation = "$InstallDir\zte-control.exe,0"
    $ScRotate.Save()
    Write-Host "[+] Desktop Shortcut created: $DesktopPath\⚡ Ротація IP (ZTE).lnk" -ForegroundColor Green

    $ScRotateSm = $WshShell.CreateShortcut("$StartMenuPath\⚡ Ротація IP (ZTE).lnk")
    $ScRotateSm.TargetPath = "powershell.exe"
    $ScRotateSm.Arguments = $PsArgs
    $ScRotateSm.WorkingDirectory = $InstallDir
    $ScRotateSm.Description = "1-Click IP Rotation with Toast Notification"
    $ScRotateSm.IconLocation = "$InstallDir\zte-control.exe,0"
    $ScRotateSm.Save()

    if (Test-Path $TaskBarPath) {
        $ScRotateTb = $WshShell.CreateShortcut("$TaskBarPath\⚡ Ротація IP (ZTE).lnk")
        $ScRotateTb.TargetPath = "powershell.exe"
        $ScRotateTb.Arguments = $PsArgs
        $ScRotateTb.WorkingDirectory = $InstallDir
        $ScRotateTb.Description = "1-Click IP Rotation with Toast Notification"
        $ScRotateTb.IconLocation = "$InstallDir\zte-control.exe,0"
        $ScRotateTb.Save()
        Write-Host "[+] Taskbar Pin Shortcut created: $TaskBarPath\⚡ Ротація IP (ZTE).lnk" -ForegroundColor Green
    }
} catch {
    Write-Host "[!] Shortcuts notice: $_" -ForegroundColor Yellow
}

# Start UI server if not running
Start-Process "$InstallDir\zte-control.exe" -ArgumentList "ui","--no-open" -WindowStyle Hidden -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

Write-Host ""
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "   ✅ INSTALLATION & SHORTCUTS COMPLETE (PURE POWERSHELL)!        " -ForegroundColor Green
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "  📂 Directory:       $InstallDir" -ForegroundColor White
Write-Host "  ⚡ 1-Click Rotate:  Ярлик '⚡ Ротація IP (ZTE)' на Робочому столі" -ForegroundColor White
Write-Host "  🔔 Notifications:   Системні сповіщення Windows увімкнено" -ForegroundColor White
Write-Host "  👉 Web Dashboard:   http://127.0.0.1:8080" -ForegroundColor White
Write-Host "==================================================================" -ForegroundColor Green
Write-Host ""

Start-Process "http://127.0.0.1:8080"
