; Echo installer hooks (Tauri NSIS). Before files are copied: close Echo's own helper programs
; (rclone / ffmpeg / ffprobe in the install folder) that may still be running, so their files can be replaced.
; Only processes started from this install folder are closed - other copies of rclone or ffmpeg are left alone.
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec `powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-Process rclone,ffmpeg,ffprobe -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like '$INSTDIR\*' } | Stop-Process -Force -ErrorAction SilentlyContinue"`
  Pop $0
  Sleep 700
!macroend
