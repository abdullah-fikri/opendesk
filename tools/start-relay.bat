@echo off
chcp 65001 >nul
title OpenDesk - Native Go Relay Gateway
cls
cd /d "%~dp0..\server-relay"
if exist server-relay.exe (
    server-relay.exe
) else (
    echo [ERROR] server-relay.exe tidak ditemukan. Menjalankan via go run...
    go run main.go
)
pause
