@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"

echo =======================================================
echo    Polymarket Agent - Background Launcher
echo =======================================================
echo.

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [INFO] Polymarket Agent is ALREADY running in the background!
    goto show_urls
)

echo Starting agent and remote hosting tunnel in background...
cscript //nologo "%~dp0run_background.vbs"
timeout /t 3 /nobreak >nul

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [SUCCESS] Agent is now running in the background!
    echo [INFO] It will continue running even after you close this terminal window.
    goto show_urls
) else (
    echo [ERROR] Agent process could not be verified. Check polymarket-agent\agent.log
    goto end
)

:show_urls
:: Ensure remote tunnel is also running if it wasn't
tasklist /FI "IMAGENAME eq cloudflared.exe" 2>nul | find /I "cloudflared.exe" >nul
if errorlevel 1 (
    echo Starting Cloudflare remote hosting tunnel...
    python scratch\manage_tunnel.py start --quiet
)

set "PUBLIC_URL="
if exist "public_url.txt" (
    set /p PUBLIC_URL=<public_url.txt
) else if exist "%~dp0public_url.txt" (
    set /p PUBLIC_URL=<%~dp0public_url.txt
)

echo.
echo =======================================================
echo    Dashboard Access Links
echo =======================================================
echo    Local Dashboard  : http://127.0.0.1:8088
if defined PUBLIC_URL (
    echo    Remote Hosting   : !PUBLIC_URL!
    echo    (Accessible anywhere from your phone or remote browser)
) else (
    echo    Remote Hosting   : Initializing... Check public_url.txt shortly
)
echo =======================================================
echo.
echo [INFO] Live output log: %~dp0polymarket-agent\agent.log
echo [INFO] To stop anytime, run: stop_background.bat

:end
echo.
pause
