@echo off
setlocal
title Public Dashboard Tunnel (Cloudflare)
echo =======================================================
echo    Public Dashboard Tunnel (Access from Phone / Remote)
echo =======================================================
echo.
cd /d "%~dp0polymarket-agent" 2>nul || cd /d "%~dp0"
python scratch\manage_tunnel.py start
echo.
pause
