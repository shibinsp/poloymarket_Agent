@echo off
setlocal
title Low-Cost Stock Scanner and Profit Harvester
echo =======================================================
echo    Low-Cost Stock Dip-Buyer ^& Profit Harvester
echo =======================================================
echo.
echo Scanning US Stocks (PLUG, NIO, LCID, NOK, SOFI)...
echo Buys low-cost stocks on dips, sells automatically for profit.
echo.
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"
python scratch\stock_profit_scanner.py
pause
