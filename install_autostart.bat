@echo off
setlocal
echo =======================================================
echo    Polymarket Agent - Windows Auto-Start Installer
echo =======================================================
echo.
echo Installing agent into Windows Startup folder...
cscript //nologo "%~dp0install_autostart.vbs"
echo.
echo [SUCCESS] Whenever you turn on your PC and log into Windows:
echo 1. The agent will automatically run in the background.
echo 2. Cloudflare remote hosting tunnel will automatically be turned ON.
echo.
echo - Local Dashboard  : http://127.0.0.1:8088
echo - Remote Dashboard : Read from public_url.txt
echo - Log file         : polymarket-agent\agent.log
echo - To remove auto-start, run: uninstall_autostart.bat
echo.
pause
