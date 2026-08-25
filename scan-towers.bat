@echo off
chcp 65001 >nul
title ZTE K12 Spectrum Scanner
echo ============================================================
echo   📡 Сканування діапазонів LTE (B3, B7, B8, B20)
echo ============================================================
echo.
rem The band sweep lives in the UIs, not the CLI. This script used to call
rem `zte-control.exe scan-towers`, which is not a subcommand and always failed.
if exist "zte-egui.exe" (
    echo [*] Відкриваю десктоп-застосунок — вкладка "Towers", кнопка "Scan bands".
    start "" "zte-egui.exe"
) else if exist "zte-control.exe" (
    echo [*] Відкриваю веб-панель — розділ "Каталог вишок", кнопка сканування.
    start "" "zte-control.exe" ui
) else (
    echo [!] zte-egui.exe / zte-control.exe не знайдено.
    pause
)
