@echo off
title Echo native prototype - build
cd /d "%~dp0"
rustup default stable-msvc >nul 2>&1
if exist "libmpv-2.dll" goto build
echo.
echo The video player needs libmpv-2.dll (mpv's player library, about 30-40 MB) in this folder.
echo Source: the standard Windows build of mpv, github.com/shinchiro/mpv-winbuild-cmake (latest release, "mpv-dev-x86_64-...7z").
choice /m "Download it now"
if errorlevel 2 (echo Put libmpv-2.dll from that .7z into this folder and run build-proto.cmd again. & pause & exit /b 1)
powershell -NoProfile -Command "$ErrorActionPreference='Stop'; $r=Invoke-RestMethod https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest; $a=$r.assets | Where-Object { $_.name -match '^mpv-dev-x86_64-\d{8}' } | Select-Object -First 1; Write-Host ('Downloading ' + $a.name + ' (' + [math]::Round($a.size/1MB) + ' MB)'); Invoke-WebRequest $a.browser_download_url -OutFile mpv-dev.7z"
if errorlevel 1 (echo Download failed - get it by hand from the page above. & pause & exit /b 1)
tar -xf mpv-dev.7z libmpv-2.dll
if not exist "libmpv-2.dll" (echo Windows could not unpack the .7z: open mpv-dev.7z in Explorer and copy libmpv-2.dll into this folder. & pause & exit /b 1)
del mpv-dev.7z
:build
cargo build --release || (echo BUILD FAILED - copy the red error lines to Claude & pause & exit /b 1)
taskkill /f /im EchoProto.exe >nul 2>&1
copy /y "target\release\echo-native-proto.exe" "EchoProto.exe" >nul
echo.
echo Built: %~dp0EchoProto.exe  - starting it...
start "" "%~dp0EchoProto.exe"
