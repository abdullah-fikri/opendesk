# OpenDesk Desktop Client (Native Rust)

Aplikasi desktop native OpenDesk berbasis Rust dan Slint UI dengan integrasi langsung ke `core-engine` (DXGI Desktop Duplication, dirty-rect detector, SendInput hardware injector).

## Fitur Utama
- **Zero Browser / Zero Electron**: Memory footprint sangat hemat (~30MB RAM vs 300MB+ Electron) dan 60+ FPS hardware capture.
- **Native Hardware Desk ID**: Terikat langsung dengan hash hardware mesin Windows.
- **Dua Arah (Host & Viewer)**: Mendukung remote dan di-remote secara simultan.
- **Buku Alamat / Riwayat**: Simpan ID rekan dengan nama alias di `%APPDATA%\OpenDesk\history.json`.
- **System Tray**: Berjalan di background notification area Windows.
- **Konsensus Persetujuan (Consent)**: Modal konfirmasi *Terima / Tolak* aman saat ada permintaan koneksi masuk.

## Cara Menjalankan
Gunakan launcher di folder `tools/`:
```text
tools\run-windows.bat
```
Atau jalankan binary langsung:
```text
builds\windows\OpenDesk.exe
```
