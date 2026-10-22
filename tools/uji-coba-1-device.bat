@echo off
chcp 65001 >nul
title OpenDesk 2.0 Native - Uji Coba Dual Device di 1 PC
cls
echo ================================================================
echo     OPENDESK 2.0 NATIVE RUST - UJI COBA 2 PERANGKAT DALAM 1 LAPTOP
echo ================================================================
echo.
echo  Membuka 2 jendela OpenDesk Native (Zero Chromium / Zero JS):
echo   [1] Jendela Kiri  : Device Asli    (ID: 145 494 982)
echo   [2] Jendela Kanan : Device Virtual (ID: 888 777 666)
echo.
echo ----------------------------------------------------------------
echo  CARA TEST:
echo  1. Pastikan relay aktif: Jalankan tools\start-relay.bat
echo  2. Di jendela KIRI, ketik ID: 888 777 666
echo  3. Klik "Hubungkan ke Rekan"
echo  4. Sesi langsung terhubung otomatis tanpa minta permission (Unattended)!
echo  5. Drag ^& Drop file apa saja dari Explorer ke jendela remote untuk kirim file!
echo ================================================================
echo.

cd /d "%~dp0.."
set "EXE=%cd%\builds\windows\OpenDesk.exe"

if not exist "%EXE%" (
    echo [ERROR] File %EXE% tidak ditemukan!
    pause
    exit /b 1
)

echo Membuka Device 1 (Kiri)...
start "" "%EXE%"

timeout /t 2 /nobreak >nul

echo Membuka Device 2 (Kanan)...
start "" "%EXE%" --id=888777666

echo.
echo Selesai! Kedua jendela OpenDesk Native sudah terbuka di layar Anda.
pause
