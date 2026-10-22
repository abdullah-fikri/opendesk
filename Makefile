# OpenDesk Build Automation Makefile
# Target platforms: Windows (Rust), Android (Gradle), Server (Go + Docker), Setup (Inno Setup)

.PHONY: all help build-windows build-windows-admin build-windows-host build-setup build-setup-admin build-setup-host build-server build-android docker-build docker-up docker-down clean

SHELL := cmd.exe

# Auto-detect Inno Setup Compiler
ISCC_PATH := $(wildcard C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe)
ifeq ($(ISCC_PATH),)
  ISCC_PATH := $(wildcard C:\Program Files (x86)\Inno Setup 6\ISCC.exe)
endif
ifeq ($(ISCC_PATH),)
  ISCC_CMD := ISCC.exe
else
  ISCC_CMD := "$(ISCC_PATH)"
endif

# Auto-detect JAVA_HOME if not present in environment
ifeq ($(JAVA_HOME),)
  STUDIO_JBR := $(wildcard C:\Program Files\Android\Android Studio\jbr)
  ifneq ($(STUDIO_JBR),)
    JAVA_HOME_CMD := set "JAVA_HOME=$(STUDIO_JBR)" &&
  else
    JAVA_HOME_CMD :=
  endif
else
  JAVA_HOME_CMD :=
endif

all: build-server build-windows build-setup

help:
	@echo ====================================================================
	@echo                      OPENDESK BUILD SYSTEM
	@echo ====================================================================
	@echo   make build-windows       - Standard User GUI client (OpenDesk.exe)
	@echo   make build-windows-admin - Admin GUI client with Server Host List (OpenDesk-Admin.exe)
	@echo   make build-windows-host  - Zero-GUI Headless Host daemon (OpenDesk-Host.exe)
	@echo   make build-setup         - Standard User Installer (OpenDesk-Setup.exe)
	@echo   make build-setup-admin   - Admin Installer (OpenDesk-Setup.exe)
	@echo   make build-setup-host    - Headless Host Installer (OpenDesk-Setup.exe)
	@echo   make build-server        - Compile Go relay gateway (server-relay.exe)
	@echo   make build-android       - Assemble Android APK (OpenDesk.apk)
	@echo   make docker-build        - Build Docker image for Relay Server
	@echo   make docker-up           - Start Relay Server with Docker Compose
	@echo   make docker-down         - Stop Relay Server container
	@echo   make all                 - Build Windows client + Setup + Server
	@echo   make clean               - Clean build artifacts & cargo caches
	@echo ====================================================================

build-windows:
	@echo [1/3] Building core-engine...
	cd core-engine && cargo build --release
	@echo [2/3] Building client-desktop (Standard User)...
	cd client-desktop && cargo build --release
	@if not exist "builds\windows" mkdir builds\windows
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\OpenDesk.exe
	@echo [3/3] Standard User build complete: builds\windows\OpenDesk.exe

build-windows-admin:
	@echo [1/3] Building core-engine...
	cd core-engine && cargo build --release
	@echo [2/3] Building client-desktop (Admin Role - Server Host List enabled)...
	cd client-desktop && cargo build --release --features admin
	@if not exist "builds\windows" mkdir builds\windows
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\OpenDesk-Admin.exe
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\OpenDesk.exe
	@echo [3/3] Admin build complete: builds\windows\OpenDesk-Admin.exe (and OpenDesk.exe)

build-windows-host:
	@echo [1/3] Building core-engine...
	cd core-engine && cargo build --release
	@echo [2/3] Building client-desktop (Headless Zero-GUI Host Only)...
	cd client-desktop && cargo build --release --features headless
	@if not exist "builds\windows" mkdir builds\windows
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\OpenDesk-Host.exe
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\OpenDesk.exe
	@echo [3/3] Headless Host build complete: builds\windows\OpenDesk-Host.exe (and OpenDesk.exe)

build-setup: build-windows
	@echo [1/1] Compiling Windows Installer (Standard Userx`)...
	@if not exist "builds\windows" mkdir builds\windows
	@if exist "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" ( "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else if exist "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ( "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else ( ISCC.exe installer\opendesk.iss )
	@echo Setup build complete: builds\windows\OpenDesk-Setup.exe

build-setup-admin: build-windows-admin
	@echo [1/1] Compiling Windows Installer (Admin Edition)...
	@if not exist "builds\windows" mkdir builds\windows
	@if exist "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" ( "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else if exist "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ( "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else ( ISCC.exe installer\opendesk.iss )
	copy /Y builds\windows\OpenDesk-Setup.exe builds\windows\OpenDesk-Admin-Setup.exe
	@echo Admin Setup build complete: builds\windows\OpenDesk-Admin-Setup.exe

build-setup-host: build-windows-host
	@echo [1/1] Compiling Windows Installer (Headless Host Edition)...
	@if not exist "builds\windows" mkdir builds\windows
	@if exist "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" ( "C:\Users\$(USERNAME)\AppData\Local\Programs\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else if exist "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ( "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\opendesk.iss ) else ( ISCC.exe installer\opendesk.iss )
	copy /Y builds\windows\OpenDesk-Setup.exe builds\windows\OpenDesk-Host-Setup.exe
	@echo Headless Host Setup build complete: builds\windows\OpenDesk-Host-Setup.exe

build-server:
	@echo [1/2] Compiling Go server-relay...
	cd server-relay && go build -ldflags="-s -w" -o server-relay.exe .
	@if not exist "builds\other" mkdir builds\other
	copy /Y server-relay\server-relay.exe builds\other\server-relay.exe
	@echo [2/2] Server build complete: builds\other\server-relay.exe

build-android:
	@echo [1/2] Assembling Android Debug APK...
	@if not defined JAVA_HOME if exist "C:\Program Files\Android\Android Studio\jbr" set "JAVA_HOME=C:\Program Files\Android\Android Studio\jbr"
	cd client-android && (if not defined JAVA_HOME if exist "C:\Program Files\Android\Android Studio\jbr" set "JAVA_HOME=C:\Program Files\Android\Android Studio\jbr") && gradlew.bat assembleDebug
	@if not exist "builds\android" mkdir builds\android
	copy /Y client-android\app\build\outputs\apk\debug\app-debug.apk builds\android\OpenDesk.apk
	@echo [2/2] Android build complete: builds\android\OpenDesk.apk

docker-build:
	@echo Building Docker image opendesk-relay:latest...
	docker build -t opendesk-relay:latest -f server-relay/Dockerfile server-relay/

docker-up:
	docker compose up -d

docker-down:
	docker compose down

clean:
	@echo Cleaning cargo and go build files...
	cd core-engine && cargo clean
	cd client-desktop && cargo clean
	del /Q server-relay\server-relay.exe 2>nul
