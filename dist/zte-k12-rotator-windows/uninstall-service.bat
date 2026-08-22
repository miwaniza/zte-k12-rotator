@echo off
title ZTE K12 Background Service Uninstaller
echo ============================================================
echo   Removing ZTE K12 Controller Windows Background Service
echo ============================================================

schtasks /End /TN "ZTEK12RotatorService" >nul 2>&1
schtasks /Delete /TN "ZTEK12RotatorService" /F >nul 2>&1

echo [+] ZTEK12RotatorService task removed from Windows.
echo.
pause
