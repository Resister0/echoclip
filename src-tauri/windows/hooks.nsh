; Echo installer hooks (Tauri NSIS).
; Before files are copied, make sure Echo's helper programs in the install folder can be replaced:
; 1. close Echo itself and its helpers (rclone / ffmpeg / ffprobe) started from this install folder,
;    a few times, because a still-closing Echo can restart rclone right after it was closed
;    (this hook runs BEFORE Tauri's own "is Echo running" check);
; 2. move any helper that is still locked out of the way (Windows allows renaming a running .exe),
;    so the new copy can always be written. The *.old files are deleted on the next update / uninstall.
; Other copies of rclone or ffmpeg on the PC are left alone.
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec `powershell -NoProfile -ExecutionPolicy Bypass -Command "$$d='$INSTDIR\*'; for($$i=0; $$i -lt 10; $$i++){ $$p = Get-Process echo,rclone,ffmpeg,ffprobe -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like $$d }; if(-not $$p){ break }; $$p | Stop-Process -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500 }"`
  Pop $0
  Sleep 500
  !insertmacro EchoMoveAside "rclone.exe"
  !insertmacro EchoMoveAside "ffmpeg.exe"
  !insertmacro EchoMoveAside "ffprobe.exe"
!macroend

!macro EchoMoveAside NAME
  Delete "$INSTDIR\tools\${NAME}.old"
  IfFileExists "$INSTDIR\tools\${NAME}" 0 +2
    Rename "$INSTDIR\tools\${NAME}" "$INSTDIR\tools\${NAME}.old"
  Delete "$INSTDIR\tools\${NAME}.old"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$INSTDIR\tools\*.old"
  RMDir "$INSTDIR\tools"
!macroend
