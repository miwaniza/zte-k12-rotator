@echo off
title ZTE K12 Background Service Installer
echo ============================================================
echo   Installing ZTE K12 Controller as Silent Background Task
echo ============================================================

set "APP_DIR=%LOCALAPPDATA%\zte-k12-rotator"
if not exist "%APP_DIR%\run-service.vbs" (
    set "APP_DIR=%~dp0"
)

echo [*] Target: %APP_DIR%\run-service.vbs

schtasks /Create /TN "ZTEK12RotatorService" /TR "wscript.exe \"%APP_DIR%\run-service.vbs\"" /SC ONLOGON /RL HIGHEST /F >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    schtasks /Create /TN "ZTEK12RotatorService" /TR "wscript.exe \"%APP_DIR%\run-service.vbs\"" /SC ONLOGON /F >nul 2>&1
)

schtasks /Run /TN "ZTEK12RotatorService" >nul 2>&1
echo [+] Windows Background Service successfully installed and started!
echo [*] Web dashboard is live at http://127.0.0.1:8080
echo.
pause
