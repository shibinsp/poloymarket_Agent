@echo off
setlocal

:: -------------------------------------------------------
::  MoneyAI — Quick Launcher
::  Double-click this file to start the trading agent.
::  Dashboard opens automatically in your browser.
:: -------------------------------------------------------

title MoneyAI Agent

:: Add Rust + cargo to PATH in case it isn't already
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo.
echo  MoneyAI Autonomous Trading Agent
echo  ----------------------------------
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

:: Check that cargo is available
where cargo >nul 2>&1
if errorlevel 1 (
    echo  ERROR: Rust / cargo not found on PATH.
    echo  Install Rust from https://rustup.rs and re-run this file.
    echo.
    pause
    exit /b 1
)

echo  Mode    : Paper Trading  (no real funds at risk)
echo  Config  : config\local.toml
echo  Dashboard: http://127.0.0.1:8088
echo.
echo  Starting agent — this may take a moment on first run...
echo  (dashboard will open in your browser automatically)
echo.

:: Open dashboard after a short delay (agent needs time to bind the port)
start "" cmd /c "timeout /t 6 /nobreak >nul && start http://127.0.0.1:8088"

:: Use pre-compiled release binary if available; fall back to cargo run
if exist "target\release\polymarket-agent.exe" (
    target\release\polymarket-agent.exe --mode paper
) else (
    cargo run --release -- --mode paper
)

echo.
echo  Agent stopped. Press any key to close.
pause >nul
