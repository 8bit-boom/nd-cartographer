@echo off
setlocal EnableExtensions
rem nd-cartographer Windows updater. Pulls the latest source, then rebuilds
rem (if Rust is installed) or re-downloads the latest prebuilt binary.
cd /d "%~dp0"
call "%~dp0scripts\windows\_common.bat"

echo ============================================================
echo   nd-cartographer - update
echo ============================================================
echo.

where git >nul 2>nul
if not %errorlevel%==0 (
    echo git was not found on PATH; cannot pull updates.
    echo Install Git for Windows from https://git-scm.com/download/win and re-run.
    goto fail
)

echo Pulling the latest source...
git pull --ff-only
if errorlevel 1 (
    echo.
    echo git pull failed. If you have local changes, commit or stash them first.
    goto fail
)
echo.

where cargo >nul 2>nul
if %errorlevel%==0 goto build

echo Rust (cargo) not found - refreshing the prebuilt Windows binary.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\windows\fetch-release.ps1"
if errorlevel 1 goto fail
goto done

:build
echo Rebuilding from source (release)...
cargo build --release --locked -p ndcart-server --bin ndcart
if errorlevel 1 goto fail

:done
echo.
echo ------------------------------------------------------------
echo   Updated. Start the studio with run.bat.
echo   Your maps in %DATA% are untouched.
echo ------------------------------------------------------------
echo.
pause
exit /b 0

:fail
echo.
pause
exit /b 1