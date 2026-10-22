@echo off
setlocal
cd /d "%~dp0.."
if exist "%cd%\builds\windows\OpenDesk.exe" (
    start "" "%cd%\builds\windows\OpenDesk.exe"
) else (
    echo [ERROR] builds\windows\OpenDesk.exe tidak ditemukan.
    pause
)
