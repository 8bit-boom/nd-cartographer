@echo off
setlocal EnableExtensions
rem nd-cartographer Windows installer. Run from the cloned repo (double-click
rem or `install.bat`). Builds from source when Rust is installed, otherwise
rem downloads the latest prebuilt Windows binary. Then use run.bat.
cd /d "%~dp0"
call "%~dp0scripts\windows\_common.bat"

echo ============================================================
echo   nd-cartographer - install
echo ============================================================
echo.

where cargo >nul 2>nul
if %errorlevel%==0 goto build

echo Rust (cargo) was not found on PATH.
echo Fetching the latest prebuilt Windows binary instead (no Rust needed).
echo To build from source, install Rust from https://rustup.rs and re-run.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\windows\fetch-release.ps1"
if errorlevel 1 goto fail
goto ready

:build
echo Building from source (release). The first build downloads dependencies
echo and can take several minutes - later builds are much faster.
echo.
cargo build --release --locked -p ndcart-server --bin ndcart
if errorlevel 1 goto fail

:ready
if not exist "%BIN%" goto fail
if not exist "%DATA%" mkdir "%DATA%"
echo.
echo ------------------------------------------------------------
echo   Installed. Your maps will live in:  %DATA%
echo   Start the studio with:              run.bat
echo   Update later with:                  update.bat
echo ------------------------------------------------------------
echo.
pause
exit /b 0

:fail
echo.
echo Install failed. If you were building from source, make sure Rust is
echo installed (https://rustup.rs) with the MSVC toolchain, then re-run.
echo.
pause
exit /b 1