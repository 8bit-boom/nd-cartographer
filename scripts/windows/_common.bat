@echo off
rem Shared setup for the Windows install/update/run scripts.
rem Called from install.bat / update.bat / run.bat; not meant to be run alone.
rem Sets ROOT (the repo root), DATA (the project data folder) and PORT.

rem The repo root is two levels up from this script (scripts\windows\).
for %%I in ("%~dp0..\..") do set "ROOT=%%~fI"
set "DATA=%ROOT%\data"
if not defined ND_CARTO_PORT set "ND_CARTO_PORT=8300"
set "PORT=%ND_CARTO_PORT%"
set "BIN=%ROOT%\target\release\ndcart.exe"
set "URL=http://127.0.0.1:%PORT%"
exit /b 0