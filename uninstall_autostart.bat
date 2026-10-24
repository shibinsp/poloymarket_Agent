@echo off
setlocal
echo =======================================================
echo    Polymarket Agent - Remove Windows Auto-Start
echo =======================================================
echo.
cscript //nologo "%~dp0uninstall_autostart.vbs"
echo.
pause
