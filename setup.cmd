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
where ffmpeg >nul 2>&1 && (echo [3/4] ffmpeg already installed - skipping) && goto mpv
echo [3/4] ffmpeg...
curl -L -o ffmpeg.zip https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip || goto fail
rmdir /s /q "%LOCALAPPDATA%\Programs\ffmpeg" 2>nul
mkdir "%LOCALAPPDATA%\Programs\ffmpeg"
tar -xf ffmpeg.zip -C "%LOCALAPPDATA%\Programs\ffmpeg" || goto fail
powershell -NoProfile -Command "$b=(Get-ChildItem \"$env:LOCALAPPDATA\Programs\ffmpeg\" -Recurse -Filter ffmpeg.exe | Select -First 1).DirectoryName; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($p -notlike \"*$b*\"){[Environment]::SetEnvironmentVariable('Path',\"$p;$b\",'User')}; Write-Host \"    ffmpeg at $b\""

:mpv
rem Echo's video player: libmpv-2.dll next to the build (prototype-native\). The same pinned file + fingerprints as
rem the release build (ci/public-build.yml, "Native app" step) - shinchiro's mpv build 20261107.
if exist "%~dp0prototype-native\libmpv-2.dll" (echo [4/4] libmpv-2.dll already there - skipping) & goto done
echo [4/4] libmpv-2.dll (the video player, about 30 MB download)...
curl -L -o mpv.7z https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261107/mpv-dev-x86_64-20261107-git-9028187538.7z || goto fail
powershell -NoProfile -Command "if((Get-FileHash mpv.7z -Algorithm SHA256).Hash -ne 'DF7C433A2D481FDAF6163B201F26ABBB0EFF0C215C51368397088C6D8F7A001B'){exit 1}" || (echo     mpv.7z fingerprint is wrong & goto fail)
tar -xf mpv.7z libmpv-2.dll || goto fail
powershell -NoProfile -Command "if((Get-FileHash libmpv-2.dll -Algorithm SHA256).Hash -ne 'C91949577AE9CC2D500208DA51F2C4A24CFA7A02D0CE6E130BF67EC738B450BB'){exit 1}" || (echo     libmpv-2.dll fingerprint is wrong & goto fail)
copy /y libmpv-2.dll "%~dp0prototype-native\libmpv-2.dll" >nul || goto fail

:done
echo.
echo Done. Close this window, open a NEW cmd window, and run build.cmd (it builds and starts prototype-native\EchoProto.exe)
pause
exit /b 0

:fail
echo.
echo SETUP FAILED at the step above - send Claude a screenshot.
pause
exit /b 1