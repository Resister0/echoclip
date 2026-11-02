@echo off
setlocal
set "DL=%LOCALAPPDATA%\clipper_setup_%RANDOM%"
mkdir "%DL%" 2>nul
cd /d "%DL%"

set "VSW=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "HAVEVC="
if exist "%VSW%" for /f "usebackq delims=" %%i in (`"%VSW%" -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "HAVEVC=1"
if defined HAVEVC (echo [1/4] MSVC Build Tools already installed - skipping) & goto rust
echo [1/4] MSVC Build Tools (C++ only, no Visual Studio app) - accept the admin prompt, takes a while...
curl -L -o vs_BuildTools.exe https://aka.ms/vs/17/release/vs_BuildTools.exe || goto fail
vs_BuildTools.exe --quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended
echo     (exit code %errorlevel% - 0 or 3010 means OK)


:rust
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (echo [2/4] Rust already installed - skipping) & goto ffmpeg
where cargo >nul 2>&1 && (echo [2/4] Rust already installed - skipping) && goto ffmpeg
echo [2/4] Rust...
curl -L -o rustup-init.exe https://win.rustup.rs/x86_64 || goto fail
rustup-init.exe -y --default-toolchain stable-msvc --profile minimal || goto fail


:ffmpeg
where ffmpeg >nul 2>&1 && (echo [3/4] ffmpeg already installed - skipping) && goto webview
echo [3/4] ffmpeg...
curl -L -o ffmpeg.zip https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip || goto fail
rmdir /s /q "%LOCALAPPDATA%\Programs\ffmpeg" 2>nul
mkdir "%LOCALAPPDATA%\Programs\ffmpeg"
tar -xf ffmpeg.zip -C "%LOCALAPPDATA%\Programs\ffmpeg" || goto fail
powershell -NoProfile -Command "$b=(Get-ChildItem \"$env:LOCALAPPDATA\Programs\ffmpeg\" -Recurse -Filter ffmpeg.exe | Select -First 1).DirectoryName; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($p -notlike \"*$b*\"){[Environment]::SetEnvironmentVariable('Path',\"$p;$b\",'User')}; Write-Host \"    ffmpeg at $b\""

:webview
echo [4/4] WebView2 runtime (skips if already installed)...
curl -L -o webview2.exe https://go.microsoft.com/fwlink/p/?LinkId=2124703 && webview2.exe /silent /install

echo.
echo Done. Close this window, open a NEW cmd window, and run build.cmd
pause
exit /b 0

:fail
echo.
echo SETUP FAILED at the step above - send Claude a screenshot.
pause
exit /b 1