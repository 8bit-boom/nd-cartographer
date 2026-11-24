@echo off
setlocal EnableExtensions
rem Wait for the studio to answer its health check, then open it in the browser.
rem Launched in the background by run.bat; not meant to be run alone.
call "%~dp0_common.bat"
set /a n=0
:wait
"%BIN%" healthcheck >nul 2>nul
if not errorlevel 1 goto open
set /a n+=1
if %n% geq 30 goto open
timeout /t 1 /nobreak >nul
goto wait
:open
start "" "%URL%"
exit /b 0