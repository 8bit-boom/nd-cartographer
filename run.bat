@echo off
setlocal EnableExtensions
rem nd-cartographer Windows launcher. Starts the studio on 127.0.0.1 and opens
rem it in your browser. Close the server window (or Ctrl+C) to stop.
rem Set ND_CARTO_PORT to use a different port (default 8300).
cd /d "%~dp0"
call "%~dp0scripts\windows\_common.bat"

if not exist "%BIN%" (
    echo The studio is not installed yet - run install.bat first.
    echo.
    pause
    exit /b 1
)
if not exist "%DATA%" mkdir "%DATA%"

echo Starting nd-cartographer on %URL% ...
echo Your maps live in %DATA%. Close this window to stop the server.
echo.

rem Open the browser once the server answers its health check (up to ~30 s).
start "" /b "%~dp0scripts\windows\open-browser.bat"

rem Run the server in the foreground so closing this window stops it.
"%BIN%" serve --listen 127.0.0.1:%PORT% --data-dir "%DATA%"

echo.
echo The server has stopped.
pause
exit /b 0