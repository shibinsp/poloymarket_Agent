@echo off
setlocal enabledelayedexpansion

:: -------------------------------------------------------
::  MoneyAI — Quick Launcher with Remote Hosting
::  Double-click this file to start the trading agent.
::  Dashboard opens automatically in your browser.
::  Remote Cloudflare tunnel is automatically started.
:: -------------------------------------------------------

title MoneyAI Agent

:: Add Rust + cargo to PATH in case it isn't already
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo.
echo =======================================================
echo    MoneyAI Autonomous Trading Agent Launcher
echo =======================================================
echo.

:: Move into the Rust project folder
cd /d "%~dp0polymarket-agent"
if errorlevel 1 (
    echo  ERROR: Could not find the polymarket-agent folder.
    echo  Make sure this file is in the root of the MoneyAI repo.
    echo.
    pause
    exit /b 1
)

:: Check that .env exists with the API key
if not exist ".env" (
    echo  WARNING: .env file not found.
    if exist ".env.example" (
        echo  Copying .env.example to .env ...
        copy .env.example .env >nul
    )
    echo  Please open .env and set your LLM_API_KEY before running.
    echo.
    pause
    exit /b 1
)

:: Start Cloudflare Remote Hosting Tunnel
echo Starting Cloudflare Remote Hosting Tunnel...
call python scratch\manage_tunnel.py start

set "PUBLIC_URL="
if exist "public_url.txt" set /p PUBLIC_URL=<public_url.txt

echo.
echo  =======================================================
echo   Mode             : Paper Trading (no real funds at risk)
echo   Local Dashboard  : http://127.0.0.1:8088
if defined PUBLIC_URL (
echo   Remote Dashboard : !PUBLIC_URL!
echo   (Phone / Remote Access: Open this link anywhere!)
)
echo  =======================================================
echo.
echo  Starting agent... (dashboard opens in your browser automatically)
echo.

:: Open dashboard after a short delay (agent needs time to bind the port)
start "" cmd /c "timeout /t 5 /nobreak >nul && start http://127.0.0.1:8088"

:: Use pre-compiled release binary if available; fall back to cargo run
if exist "target\release\polymarket-agent.exe" (
    target\release\polymarket-agent.exe --mode paper
) else (
    cargo run --release -- --mode paper
)

echo.
echo  Stopping remote tunnel...
call python scratch\manage_tunnel.py stop

echo.
echo  Agent stopped. Press any key to close.
pause >nul
