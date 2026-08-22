# ==============================================================================
#  ZTE K12 Mobile Controller & IP Rotator - 1-Click Windows Installer & Service
#  Usage: irm https://raw.githubusercontent.com/miwaniza/zte-k12-rotator/main/install.ps1 | iex
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

# 5. Register Windows Background Service (Scheduled Task on Logon)
Write-Host "[*] Configuring Windows Background Service (Auto-Start on Logon)..." -ForegroundColor Cyan
try {
    $ExePath = "$InstallDir\zte-control.exe"
    $TaskName = "ZTEK12RotatorService"
    $TaskAction = "\"$ExePath\" ui --no-open"
    
    # Try creating elevated task, fallback to user level
    $p = Start-Process -FilePath "schtasks" -ArgumentList "/Create /TN `"$TaskName`" /TR `"$TaskAction`" /SC ONLOGON /RL HIGHEST /F" -Wait -PassThru -NoNewWindow
    if ($p.ExitCode -ne 0) {
        Start-Process -FilePath "schtasks" -ArgumentList "/Create /TN `"$TaskName`" /TR `"$TaskAction`" /SC ONLOGON /F" -Wait -NoNewWindow
    }
    
    # Start the service task
    Start-Process -FilePath "schtasks" -ArgumentList "/Run /TN `"$TaskName`"" -Wait -NoNewWindow
    Write-Host "[+] Windows Background Service registered & started!" -ForegroundColor Green
} catch {
    Write-Host "[!] Service registration warning: $_" -ForegroundColor Yellow
}

# 6. Create Desktop & Start Menu Shortcuts
try {
    $WshShell = New-Object -ComObject WScript.Shell
    $DesktopPath = [Environment]::GetFolderPath("Desktop")
    $Shortcut = $WshShell.CreateShortcut("$DesktopPath\ZTE K12 Controller.lnk")
    $Shortcut.TargetPath = "$InstallDir\start-dashboard.bat"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "ZTE K12 4G LTE Controller & IP Rotator"
    $Shortcut.Save()
    Write-Host "[+] Desktop Shortcut created: $DesktopPath\ZTE K12 Controller.lnk" -ForegroundColor Green

    $StartMenuPath = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
    $SmShortcut = $WshShell.CreateShortcut("$StartMenuPath\ZTE K12 Controller.lnk")
    $SmShortcut.TargetPath = "$InstallDir\start-dashboard.bat"
    $SmShortcut.WorkingDirectory = $InstallDir
    $SmShortcut.Description = "ZTE K12 4G LTE Controller & IP Rotator"
    $SmShortcut.Save()
} catch {
    Write-Host "[!] Could not create shortcut automatically: $_" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "   ✅ INSTALLATION & SERVICE SETUP COMPLETE!                      " -ForegroundColor Green
Write-Host "==================================================================" -ForegroundColor Green
Write-Host "  📂 Directory:    $InstallDir" -ForegroundColor White
Write-Host "  ⚙️ Service:      ZTEK12RotatorService (Running in background)" -ForegroundColor White
Write-Host "  👉 Dashboard:    http://127.0.0.1:8080" -ForegroundColor White
Write-Host "  ⚡ CLI Command:  zte-control status  (or rotate / reconnect)" -ForegroundColor White
Write-Host "==================================================================" -ForegroundColor Green
Write-Host ""

Start-Process "http://127.0.0.1:8080"
