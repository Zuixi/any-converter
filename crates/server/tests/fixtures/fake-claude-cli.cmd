@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0fake-claude-cli.ps1" %*
