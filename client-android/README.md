# OpenDesk Android Native Client

Project aplikasi Android native untuk OpenDesk dengan fitur:
- **`OpenDeskAccessibilityService`**: Menyimulasikan klik dan gesture di OS Android dari laptop remote.
- **`ScreenCaptureService`**: Menangkap layar HP via MediaProjection dan melakukan stream real-time ke gateway.
- **Hardware Desk ID**: Terikat dengan `Settings.Secure.ANDROID_ID`.

## Cara Compile ke APK

1. Buka folder ini di **Android Studio**:
   ```text
   D:\opendesk\android
   ```
2. Biarkan Gradle melakukan sinkronisasi otomatis.
3. Klik menu **Build** -> **Build Bundle(s) / APK(s)** -> **Build APK(s)**.
4. File APK akan dihasilkan di:
   ```text
   app\build\outputs\apk\debug\app-debug.apk
   ```
5. Pasang (install) di HP Android Anda.
6. Berikan izin:
   - **Layanan Aksesibilitas (Accessibility)**: Aktifkan OpenDesk di Pengaturan Aksesibilitas Android.
   - **Rekam Layar (MediaProjection)**: Izinkan saat jendela konfirmasi muncul.
