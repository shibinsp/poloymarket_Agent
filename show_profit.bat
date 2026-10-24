@echo off
setlocal
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"
python scratch\check_profit.py
echo.
pause
