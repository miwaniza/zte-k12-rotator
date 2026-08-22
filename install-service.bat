@echo off
title ZTE K12 Background Service Installer
echo ============================================================
echo   Installing ZTE K12 Controller as Windows Background Task
echo ============================================================

set "APP_DIR=%LOCALAPPDATA%\zte-k12-rotator"
if not exist "%APP_DIR%\zte-control.exe" (
    set "APP_DIR=%~dp0"
)

echo [*] Target executable: %APP_DIR%\zte-control.exe

schtasks /Create /TN "ZTEK12RotatorService" /TR "\"%APP_DIR%\zte-control.exe\" ui --no-open" /SC ONLOGON /RL HIGHEST /F >nul 2>&1
if %ERRORLEVEL% EQU 0 (
    echo [+] Successfully registered Windows Background Task: ZTEK12RotatorService
    echo [*] Starting service now...
    schtasks /Run /TN "ZTEK12RotatorService" >nul 2>&1
    echo [!] Service is now running in the background at http://127.0.0.1:8080
) else (
    echo [-] Failed to register task with elevated privileges. Trying standard user logon...
    schtasks /Create /TN "ZTEK12RotatorService" /TR "\"%APP_DIR%\zte-control.exe\" ui --no-open" /SC ONLOGON /F >nul 2>&1
    schtasks /Run /TN "ZTEK12RotatorService" >nul 2>&1
    echo [+] Registered as user task.
)

echo.
echo Press any key to exit...
pause >nul
