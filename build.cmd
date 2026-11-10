@echo off
rem Echo - Windows build. Echo 2.0 is the native app only (prototype-native: Slint + mpv); the old Tauri app is no
rem longer built (William, 10 Nov 2026 - its source stays in git history and the private release tauri-1.0.71-backup).
call "%~dp0prototype-native\build-proto.cmd"
