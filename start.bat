@echo off
setlocal enabledelayedexpansion
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\w64devkit\bin;%PATH%"

title Polymarket Autonomous Trading Agent

echo =======================================================
echo     Polymarket Autonomous Trading Agent Launcher
echo =======================================================
echo.

cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"

:: Check for .env file
if not exist ".env" (
    echo [WARNING] .env file not found!
    if exist ".env.example" (
        echo Creating .env from .env.example...
        copy .env.example .env
        echo [INFO] Please edit .env with your LLM_API_KEY or ANTHROPIC_API_KEY before running.
        echo.
    )
)

:: If argument passed (e.g. start.bat paper, start.bat background, start.bat stop)
if "%1"=="paper" goto run_paper
if "%1"=="background" goto run_background
if "%1"=="bg" goto run_background
if "%1"=="stop" goto stop_background
if "%1"=="status" goto check_status
if "%1"=="autostart" goto install_autostart
if "%1"=="live" goto run_live
if "%1"=="backtest" goto run_backtest
if "%1"=="dry-run" goto run_dry_run
if "%1"=="dryrun" goto run_dry_run

:menu
echo Select operation mode:
echo   [1] Paper Trading Mode (Interactive Foreground - Remote Hosting ON)
echo   [2] Run in Background (Silent / Detached - Remote Hosting ON)
echo   [3] Stop Background Agent & Remote Tunnel
echo   [4] Check Status & Open Web Dashboard (Local + Remote Links)
echo   [5] Show Profit & Performance Report (Track Earnings)
echo   [6] Setup Auto-Start on Windows Login (Runs automatically offline/on boot)
echo   [7] Remove Auto-Start from Windows
echo   [8] Dry Run / Connectivity Validation
echo   [9] Backtest Mode (Simulated historical runs)
echo   [10] Live Trading Mode (REAL FUNDS / Polygon USDC)
echo   [11] Exit
echo.
set /p choice="Enter your choice [1-11] (default: 1): "

if "%choice%"=="" set choice=1
if /i "%choice%"=="p" goto show_profit
if "%choice%"=="1" goto run_paper
if "%choice%"=="2" goto run_background
if "%choice%"=="3" goto stop_background
if "%choice%"=="4" goto check_status
if "%choice%"=="5" goto show_profit
if "%choice%"=="6" goto install_autostart
if "%choice%"=="7" goto uninstall_autostart
if "%choice%"=="8" goto run_dry_run
if "%choice%"=="9" goto run_backtest
if "%choice%"=="10" goto run_live
if "%choice%"=="11" goto exit_app

echo Invalid selection. Defaulting to Paper Trading Mode.
goto run_paper

:run_paper
echo.
echo =======================================================
echo  Starting Cloudflare Remote Hosting Tunnel...
echo =======================================================
call python scratch\manage_tunnel.py start

set "PUBLIC_URL="
if exist "public_url.txt" set /p PUBLIC_URL=<public_url.txt

echo.
echo =======================================================
echo  Starting Agent in PAPER TRADING MODE (Foreground)
echo  Local Dashboard  : http://127.0.0.1:8088
if defined PUBLIC_URL (
echo  Remote Dashboard : !PUBLIC_URL!
echo  (Phone / External Access: Open this link anywhere!)
)
echo =======================================================
echo.

:: Open local dashboard in browser
start "" cmd /c "timeout /t 5 /nobreak >nul && start http://127.0.0.1:8088"

if exist "target\release\polymarket-agent.exe" (
    target\release\polymarket-agent.exe --mode paper
) else (
    cargo run --release -- --mode paper
)

echo.
echo Stopping remote tunnel...
call python scratch\manage_tunnel.py stop
goto end

:run_background
echo.
echo =======================================================
echo  Starting Agent in BACKGROUND (Silent / Detached Mode)
echo =======================================================
tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [INFO] Polymarket Agent is ALREADY running in the background!
    goto show_bg_urls
)

cscript //nologo "%~dp0run_background.vbs"
timeout /t 4 /nobreak >nul

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [SUCCESS] Agent is now running in the background!
    echo [INFO] It will keep running even if you close this window.
    echo [INFO] Output log: %~dp0polymarket-agent\agent.log
    goto show_bg_urls
) else (
    echo [ERROR] Agent failed to start. Check polymarket-agent\agent.log
    goto end
)

:show_bg_urls
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
echo  Web Dashboard Links:
echo  Local Dashboard  : http://127.0.0.1:8088
if defined PUBLIC_URL (
echo  Remote Dashboard : !PUBLIC_URL!
echo  (Access from your phone or any external browser)
)
echo =======================================================
goto end

:stop_background
echo.
call "%~dp0stop_background.bat"
goto end

:check_status
echo.
echo =======================================================
echo  Agent Status Check
echo =======================================================
tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if not errorlevel 1 (
    echo [STATUS] Polymarket Agent process is RUNNING.
) else (
    echo [STATUS] Polymarket Agent process is NOT currently running.
)

tasklist /FI "IMAGENAME eq cloudflared.exe" 2>nul | find /I "cloudflared.exe" >nul
if not errorlevel 1 (
    echo [STATUS] Cloudflare Remote Tunnel is RUNNING.
) else (
    echo [STATUS] Cloudflare Remote Tunnel is NOT running.
    echo [ACTION] Starting remote hosting tunnel...
    python scratch\manage_tunnel.py start --quiet
)

set "PUBLIC_URL="
if exist "public_url.txt" (
    set /p PUBLIC_URL=<public_url.txt
) else if exist "%~dp0public_url.txt" (
    set /p PUBLIC_URL=<%~dp0public_url.txt
)

echo.
echo Local Dashboard  : http://127.0.0.1:8088
if defined PUBLIC_URL (
echo Remote Dashboard : !PUBLIC_URL!
)
echo.
echo Opening Web Dashboard in browser: http://127.0.0.1:8088
start http://127.0.0.1:8088
goto end

:show_profit
echo.
python "%~dp0polymarket-agent\scratch\check_profit.py"
goto end

:install_autostart
echo.
call "%~dp0install_autostart.bat"
goto end

:uninstall_autostart
echo.
call "%~dp0uninstall_autostart.bat"
goto end

:run_dry_run
echo.
echo =======================================================
echo  Running DRY RUN Validation
echo =======================================================
cargo run -- --dry-run
goto end

:run_backtest
echo.
echo =======================================================
echo  Running BACKTEST Mode
echo =======================================================
cargo run -- --mode backtest
goto end

:run_live
echo.
echo =======================================================
echo  WARNING: STARTING LIVE TRADING MODE (REAL FUNDS)
echo =======================================================
set /p confirm="Are you sure you want to trade with real funds? (y/N): "
if /i not "%confirm%"=="y" (
    echo Live trading cancelled.
    goto menu
)
call python scratch\manage_tunnel.py start
if exist "target\release\polymarket-agent.exe" (
    target\release\polymarket-agent.exe --mode live
) else (
    cargo run --release -- --mode live
)
call python scratch\manage_tunnel.py stop
goto end

:exit_app
echo Exiting.
goto end

:end
echo.
pause
