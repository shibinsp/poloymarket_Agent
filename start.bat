@echo off
setlocal enabledelayedexpansion

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
        echo [INFO] Please edit .env with your LLM_API_KEY (or ANTHROPIC_API_KEY) before running.
        echo.
    )
)

:: If argument passed (e.g. start.bat paper, start.bat dry-run)
if "%1"=="paper" goto run_paper
if "%1"=="live" goto run_live
if "%1"=="backtest" goto run_backtest
if "%1"=="dry-run" goto run_dry_run
if "%1"=="dryrun" goto run_dry_run

:menu
echo Select operation mode:
echo   [1] Paper Trading Mode (Simulated funds, live market data - Recommended)
echo   [2] Dry Run / Connectivity Validation
echo   [3] Backtest Mode (Simulated historical runs)
echo   [4] Live Trading Mode (REAL FUNDS / Polygon USDC)
echo   [5] Exit
echo.
set /p choice="Enter your choice [1-5] (default: 1): "

if "%choice%"=="" set choice=1
if "%choice%"=="1" goto run_paper
if "%choice%"=="2" goto run_dry_run
if "%choice%"=="3" goto run_backtest
if "%choice%"=="4" goto run_live
if "%choice%"=="5" goto exit_app

echo Invalid selection. Defaulting to Paper Trading Mode.
goto run_paper

:run_paper
echo.
echo =======================================================
echo  Starting Agent in PAPER TRADING MODE
echo  Dashboard: http://127.0.0.1:8080
echo =======================================================
cargo run --release -- --mode paper
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
cargo run --release -- --mode live
goto end

:exit_app
echo Exiting.
goto end

:end
echo.
pause
