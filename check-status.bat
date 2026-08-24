@echo off
chcp 65001 >nul
title ZTE K12 Status
if exist "zte-control.exe" (
    zte-control.exe status
) else (
    python tools\cell_control.py status
)
echo.
pause
