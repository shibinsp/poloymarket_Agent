@echo off
setlocal
echo =======================================================
echo    Polymarket Agent - Stop Background Process
echo =======================================================
echo.

tasklist /FI "IMAGENAME eq polymarket-agent.exe" 2>nul | find /I "polymarket-agent.exe" >nul
if errorlevel 1 (
    echo [INFO] No running polymarket-agent.exe process found.
    echo.
    goto end
)

echo Stopping polymarket-agent.exe...
taskkill /F /IM polymarket-agent.exe 2>nul
if not errorlevel 1 (
    echo [SUCCESS] Agent background process terminated cleanly.
) else (
    echo [ERROR] Failed to terminate agent process.
)

:end
echo.
pause
