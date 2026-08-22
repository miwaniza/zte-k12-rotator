@echo off
chcp 65001 >nul
title ZTE K12 Spectrum Scanner
echo ============================================================
echo   📡 Сканування всіх діапазонів LTE (B3, B7, B8, B20)...
echo ============================================================
echo.
if exist "zte-control.exe" (
    zte-control.exe scan-towers
) else (
    python tools\cell_control.py
)
echo.
pause
