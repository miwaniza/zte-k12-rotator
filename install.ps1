# ==============================================================================
#  ZTE K12 Mobile Controller & IP Rotator - 1-Click Windows Installer & Service
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
    Expand-Archive -Path $TempZip -DestinationPath "$env:TEMP\zte_extract" -Force
    Copy-Item -Path "$env:TEMP\zte_extract\dist\zte-k12-rotator-windows\*" -Destination $InstallDir -Recurse -Force
    Remove-Item -Path "$env:TEMP\zte_extract" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -Path $TempZip -Force -ErrorAction SilentlyContinue
} catch {
    Expand-Archive -Path $TempZip -DestinationPath $InstallDir -Force
    Remove-Item -Path $TempZip -Force -ErrorAction SilentlyContinue
}

# 4. Add to User PATH
$UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -notlike "*$InstallDir*") {
    Write-Host "[+] Adding $InstallDir to User PATH..." -ForegroundColor Green
    [Environment]::SetEnvironmentVariable("PATH", "$UserPath;$InstallDir", "User")
    $env:PATH += ";$InstallDir"
}

# 5. Register Windows Background Service (Silent VBS Task on Logon)
Write-Host "[*] Configuring Silent Windows Background Service..." -ForegroundColor Cyan
try {
    $VbsPath = "$InstallDir\run-service.vbs"
    $TaskName = "ZTEK12RotatorService"
    $TrValue = "wscript.exe `"$VbsPath`""

    & schtasks /Create /TN $TaskName /TR $TrValue /SC ONLOGON /RL HIGHEST /F 2>$null
    if ($LASTEXITCODE -ne 0) {
        & schtasks /Create /TN $TaskName /TR $TrValue /SC ONLOGON /F 2>$null
    }
    & schtasks /Run /TN $TaskName 2>$null
    Write-Host "[+] Windows Background Service registered & started silently!" -ForegroundColor Green
} catch {
    Write-Host "[!] Service registration notice: $_" -ForegroundColor Yellow
}

# 6. Create Desktop & Taskbar / Start Menu Shortcuts
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

    # Shortcut 2: 1-Click Fast Rotation with System Notification
    $ScRotate = $WshShell.CreateShortcut("$DesktopPath\⚡ Ротація IP (ZTE).lnk")
    $ScRotate.TargetPath = "wscript.exe"
    $ScRotate.Arguments = "`"$InstallDir\rotate-silent.vbs`""
    $ScRotate.WorkingDirectory = $InstallDir
    $ScRotate.Description = "1-Click IP Rotation with System Notification"
    $ScRotate.IconLocation = "$InstallDir\zte-control.exe,0"
    $ScRotate.Save()
    Write-Host "[+] Desktop Shortcut created: $DesktopPath\⚡ Ротація IP (ZTE).lnk" -ForegroundColor Green

    $ScRotateSm = $WshShell.CreateShortcut("$StartMenuPath\⚡ Ротація IP (ZTE).lnk")
    $ScRotateSm.TargetPath = "wscript.exe"
    $ScRotateSm.Arguments = "`"$InstallDir\rotate-silent.vbs`""
    $ScRotateSm.WorkingDirectory = $InstallDir
    $ScRotateSm.Description = "1-Click IP Rotation with System Notification"
    $ScRotateSm.IconLocation = "$InstallDir\zte-control.exe,0"
    $ScRotateSm.Save()

    # Taskbar pin (if folder exists)
    if (Test-Path $TaskBarPath) {
        $ScRotateTb = $WshShell.CreateShortcut("$TaskBarPath\⚡ Ротація IP (ZTE).lnk")
        $ScRotateTb.TargetPath = "wscript.exe"
        $ScRotateTb.Arguments = "`"$InstallDir\rotate-silent.vbs`""
        $ScRotateTb.WorkingDirectory = $InstallDir
        $ScRotateTb.Description = "1-Click IP Rotation with System Notification"
        $ScRotateTb.IconLocation = "$InstallDir\zte-control.exe,0"
        $ScRotateTb.Save()
        Write-Host "[+] Taskbar Pin Shortcut created: $TaskBarPath\⚡ Ротація IP (ZTE).lnk" -ForegroundColor Green
    }
} catch {
    Write-Host "[!] Shortcuts notice: $_" -ForegroundColor Yellow
}

# Wait for service to initialize
Start-Sleep -Seconds 1

Write-Host ""
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "   ✅ INSTALLATION & SHORTCUTS COMPLETE!                          " -ForegroundColor Green
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "  📂 Directory:       $InstallDir" -ForegroundColor White
Write-Host "  ⚡ 1-Click Rotate:  Ярлик '⚡ Ротація IP (ZTE)' на Робочому столі" -ForegroundColor White
Write-Host "  🔔 Notifications:   Системні сповіщення Windows увімкнено" -ForegroundColor White
Write-Host "  👉 Web Dashboard:   http://127.0.0.1:8080" -ForegroundColor White
Write-Host "==================================================================" -ForegroundColor Green
Write-Host ""

Start-Process "http://127.0.0.1:8080"
