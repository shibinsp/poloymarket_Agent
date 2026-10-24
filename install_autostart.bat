@echo off
setlocal
echo =======================================================
echo    Polymarket Agent - Windows Auto-Start Installer
echo =======================================================
echo.
echo Installing agent into Windows Startup folder...
cscript //nologo "%~dp0install_autostart.vbs"
echo.
echo [SUCCESS] Whenever you turn on your PC and log into Windows,
echo the agent will automatically run in the background.
echo - Web Dashboard: http://127.0.0.1:8080
echo - Log file: polymarket-agent\agent.log
echo - To remove auto-start, run: uninstall_autostart.bat
echo.
pause
