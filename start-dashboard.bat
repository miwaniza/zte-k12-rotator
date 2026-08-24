@echo off
chcp 65001 >nul
title ZTE K12 Smart Tower Rotator
echo ============================================================
echo   🚀 Запуск ZTE K12 Панелі керування та Ротатора Сот
echo   Веб-інтерфейс: http://127.0.0.1:8080
echo ============================================================
echo.
if exist "zte-control.exe" (
    zte-control.exe ui
) else (
    echo [!] zte-control.exe не знайдено, запуск через Python...
    python tools\cell_control.py
)
pause
