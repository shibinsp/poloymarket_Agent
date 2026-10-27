@echo off
setlocal
title Polymarket Autonomous Profit Harvester
echo =======================================================
echo    Polymarket Autonomous Profit Harvester (Live)
echo =======================================================
echo.
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"
python scratch\profit_harvester_daemon.py
pause
