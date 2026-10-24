@echo off
setlocal
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"

echo =======================================================
echo    Polymarket Agent - Background Launcher
echo =======================================================
echo.

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [INFO] Polymarket Agent is ALREADY running in the background!
    echo [INFO] Web Dashboard: http://127.0.0.1:8080
    echo [INFO] Log file: polymarket-agent\agent.log
    echo.
    goto end
)

echo Starting agent in silent background mode...
cscript //nologo "%~dp0run_background.vbs"
timeout /t 2 /nobreak >nul

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [SUCCESS] Agent is now running in the background!
    echo [INFO] It will continue running even after you close this terminal window.
    echo [INFO] Web Dashboard: http://127.0.0.1:8080
    echo [INFO] Live output log: %~dp0polymarket-agent\agent.log
    echo [INFO] To stop it anytime, run: stop_background.bat
) else (
    echo [ERROR] Agent process could not be verified. Check polymarket-agent\agent.log
)

:end
echo.
pause
