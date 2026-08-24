@echo off
chcp 65001 >nul
title ZTE K12 Trigger Rotation
echo ============================================================
echo   ⚡ РОТАЦІЯ НА НАСТУПНУ ВИШКУ ТА ОНОВЛЕННЯ IP
echo ============================================================
echo.
if exist "zte-control.exe" (
    rem Exits non-zero when the address did not actually change.
    zte-control.exe rotate
    if errorlevel 1 (
        echo.
        echo [!] Ротація завершилась без зміни IP.
    ) else (
        echo.
        echo [OK] Ротацію виконано, IP змінено.
    )
) else (
    rem The API needs POST + the XHR header; a plain GET is refused with 405.
    curl -sS -X POST -H "X-Requested-With: XMLHttpRequest" http://127.0.0.1:8080/api/rotate
)
echo.
timeout /t 5
