@echo off
chcp 65001 >nul
title ZTE K12 Trigger Rotation
echo ============================================================
echo   ⚡ РОТАЦІЯ НА НАСТУПНУ ВИШКУ ТА ОНОВЛЕННЯ IP
echo ============================================================
echo.
if exist "zte-control.exe" (
    zte-control.exe rotate
) else (
    curl -sS http://127.0.0.1:8080/api/rotate
)
echo.
echo [OK] Ротацію виконано!
timeout /t 5
