@echo off
setlocal enabledelayedexpansion
title OpenDesk Clean Uninstaller

echo ========================================================
echo               OPENDESK CLEAN UNINSTALLER                
echo ========================================================
echo.

:: 1. Tutup proses OpenDesk jika masih aktif
echo [1/4] Menghentikan proses OpenDesk...
taskkill /F /IM OpenDesk.exe >nul 2>&1
taskkill /F /IM opendesk-client.exe >nul 2>&1

:: 2. Cari uninstaller bawaan Inno Setup
set "UNINST="
if exist "%LOCALAPPDATA%\Programs\OpenDesk\unins000.exe" set "UNINST=%LOCALAPPDATA%\Programs\OpenDesk\unins000.exe"
if exist "%ProgramFiles%\OpenDesk\unins000.exe" set "UNINST=%ProgramFiles%\OpenDesk\unins000.exe"
if exist "%ProgramFiles(x86)%\OpenDesk\unins000.exe" set "UNINST=%ProgramFiles(x86)%\OpenDesk\unins000.exe"

if defined UNINST (
    echo [2/4] Menjalankan uninstaller resmi: !UNINST!...
    "!UNINST!" /SILENT
    timeout /t 2 /nobreak >nul
) else (
    echo [2/4] Uninstaller wizard tidak ditemukan di Program Files, membersihkan file instalasi...
    if exist "%LOCALAPPDATA%\Programs\OpenDesk" rmdir /s /q "%LOCALAPPDATA%\Programs\OpenDesk" >nul 2>&1
    if exist "%ProgramFiles%\OpenDesk" rmdir /s /q "%ProgramFiles%\OpenDesk" >nul 2>&1
)

:: 3. Hapus Shortcut Desktop & Start Menu
echo [3/4] Menghapus pintasan Desktop dan Start Menu...
del /f /q "%USERPROFILE%\Desktop\OpenDesk.lnk" >nul 2>&1
del /f /q "%PUBLIC%\Desktop\OpenDesk.lnk" >nul 2>&1
del /f /q "%ONEDRIVE%\Desktop\OpenDesk.lnk" >nul 2>&1
del /f /q "%APPDATA%\Microsoft\Windows\Start Menu\Programs\OpenDesk.lnk" >nul 2>&1
del /f /q "%APPDATA%\Microsoft\Windows\Start Menu\Programs\Uninstall OpenDesk.lnk" >nul 2>&1
del /f /q "%PROGRAMDATA%\Microsoft\Windows\Start Menu\Programs\OpenDesk.lnk" >nul 2>&1

:: 4. Bersihkan data AppData / Cache / Riwayat koneksi
echo [4/4] Menghapus data konfigurasi OpenDesk (%APPDATA%\OpenDesk)...
if exist "%APPDATA%\OpenDesk" (
    rmdir /s /q "%APPDATA%\OpenDesk" >nul 2>&1
)

echo.
echo ========================================================
echo     BERHASIL! OpenDesk telah bersih dihapus dari sistem.
echo ========================================================
pause
