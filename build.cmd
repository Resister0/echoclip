@echo off
title Echo - build
taskkill /f /im Echo.exe >nul 2>&1
taskkill /f /im Clipper.exe >nul 2>&1
cd /d "%~dp0src-tauri"
rustup default stable-msvc >nul 2>&1
rem touch main.rs so the UI is always re-embedded
pushd src & copy /b main.rs +,, >nul & popd
cargo build --release || (echo BUILD FAILED - copy the red error lines to Claude & pause & exit /b 1)
rem Echo may still be running (tray) or was started again during the build: stop it and retry a few times
set tries=0
:copyagain
taskkill /f /im Echo.exe >nul 2>&1
timeout /t 1 /nobreak >nul
copy /y "target\release\echo.exe" "%~dp0Echo.exe" >nul 2>&1 && goto copied
set /a tries+=1
if %tries% lss 3 goto copyagain
rem still locked (e.g. Echo running as admin, or antivirus): a locked exe can still be renamed
if exist "%~dp0Echo.old.exe" del /f "%~dp0Echo.old.exe" >nul 2>&1
move /y "%~dp0Echo.exe" "%~dp0Echo.old.exe" >nul 2>&1
copy /y "target\release\echo.exe" "%~dp0Echo.exe" >nul 2>&1 && (echo Note: the old Echo was still running - restart Echo to use the new one & goto copied)
echo COULD NOT REPLACE Echo.exe - close Echo (tray icon ^> Quit) and run build.cmd again
pause & exit /b 1
:copied
if exist "%~dp0Clipper.exe" del "%~dp0Clipper.exe"
if exist "target\release\clipper.exe" del "target\release\clipper.exe" >nul 2>&1
rem give this folder the Echo icon in Explorer
if not exist "%~dp0desktop.ini" (
  (echo [.ShellClassInfo]& echo IconResource=Echo.exe,0) > "%~dp0desktop.ini"
  attrib +s +h "%~dp0desktop.ini"
  attrib +r "%~dp0."
)
echo.
echo Built: %~dp0Echo.exe
pause
