@echo off
rem Echo - save your changes to GitHub WITHOUT a release (run when you start and before you stop on this PC)
rem Sonela master (..\CLAUDE.md, its own private repo Resister0/sonela-dev): save + get it first, whichever app is synced
if exist "%~dp0..\.git" (
  git -C "%~dp0.." add -A
  git -C "%~dp0.." diff --cached --quiet || git -C "%~dp0.." commit -q -m "Sync (Windows)"
  git -C "%~dp0.." pull -q --rebase --autostash
  git -C "%~dp0.." push -q
)
call "%~dp0publish.cmd" sync
