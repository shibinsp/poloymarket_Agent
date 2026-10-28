@echo off
setlocal
echo =======================================================
echo    Polymarket Agent - Stop Background Process
echo =======================================================
echo.

echo Stopping polymarket-agent.exe...
taskkill /F /IM polymarket-agent.exe 2>nul
if not errorlevel 1 (
    echo [SUCCESS] Agent background process terminated.
) else (
    echo [INFO] No running polymarket-agent.exe found.
)

echo Stopping Cloudflare remote tunnel (cloudflared.exe)...
taskkill /F /IM cloudflared.exe 2>nul
if not errorlevel 1 (
    echo [SUCCESS] Remote hosting tunnel stopped.
) else (
    echo [INFO] No running cloudflared.exe found.
)

del /q "%~dp0public_url.txt" 2>nul
del /q "%~dp0polymarket-agent\public_url.txt" 2>nul

echo.
echo All background services stopped cleanly.
echo.
pause
