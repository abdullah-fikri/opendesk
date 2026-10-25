# OpenDesk developer tasks. Windows uses cmd.exe, Linux (and other POSIX systems) uses sh; the targets differ
# per OS (`make windows` / `make linux`), the shared ones (test, lint, relay, android, docker-*) exist on both.
# CI runs the same commands; see .github/workflows. Run `make help` for the list.

VERSION := $(file < VERSION)

ifeq ($(OS),Windows_NT)
# ---- Windows (cmd.exe) ---------------------------------------------------------------------

SHELL := cmd.exe

# Inno Setup compiler: per-user install, machine-wide install, or PATH.
ISCC := $(firstword $(wildcard $(LOCALAPPDATA)/Programs/Inno\ Setup\ 6/ISCC.exe) $(wildcard C:/Program\ Files\ (x86)/Inno\ Setup\ 6/ISCC.exe) ISCC.exe)
# Gradle needs a JDK; fall back to the one bundled with Android Studio.
ifeq ($(JAVA_HOME),)
  export JAVA_HOME := C:\Program Files\Android\Android Studio\jbr
endif

.PHONY: help all windows windows-admin windows-host setup setup-admin setup-host relay run-relay android \
	docker-build docker-up docker-down test lint clean

help:
	@echo OpenDesk $(VERSION)
	@echo.
	@echo   make windows        Desktop client, standard edition   -^> builds\windows\OpenDesk.exe
	@echo   make windows-admin  Desktop client, admin edition      -^> builds\windows\OpenDesk-Admin.exe
	@echo   make windows-host   Desktop client, headless host      -^> builds\windows\OpenDesk-Host.exe
	@echo   make setup          Installer for the standard edition -^> builds\windows\OpenDesk-Setup.exe
	@echo   make setup-admin    Installer for the admin edition
	@echo   make setup-host     Installer for the headless host
	@echo   make relay          Relay server binary                -^> builds\relay\opendesk-relay.exe
	@echo   make run-relay      Run the relay from source (needs PostgreSQL, see server-relay\README.md)
	@echo   make android        Android debug APK                  -^> builds\android\OpenDesk.apk
	@echo   make docker-build   Relay Docker image opendesk-relay:$(VERSION)
	@echo   make docker-up      Start the relay with Docker Compose
	@echo   make docker-down    Stop it
	@echo   make test           All unit tests
	@echo   make lint           Formatters and linters (clippy, go vet, gofmt)
	@echo   make clean          Remove build output

all: relay windows setup

# ---- Desktop -------------------------------------------------------------------------------

# $(1) = Cargo features ("" for the standard edition), $(2) = output file name.
define build_windows
	cd client-desktop && cargo build --release --locked $(1)
	@if not exist builds\windows mkdir builds\windows
	copy /Y client-desktop\target\release\opendesk-client.exe builds\windows\$(2)
endef

windows:
	$(call build_windows,,OpenDesk.exe)

windows-admin:
	$(call build_windows,--features admin,OpenDesk-Admin.exe)
	copy /Y builds\windows\OpenDesk-Admin.exe builds\windows\OpenDesk.exe

windows-host:
	$(call build_windows,--features headless,OpenDesk-Host.exe)
	copy /Y builds\windows\OpenDesk-Host.exe builds\windows\OpenDesk.exe

# The installer always packages builds\windows\OpenDesk.exe; $(1) = name of the produced setup.
define build_setup
	"$(ISCC)" /DMyAppVersion=$(VERSION) installer\opendesk.iss
	@if not "$(1)"=="OpenDesk-Setup.exe" copy /Y builds\windows\OpenDesk-Setup.exe builds\windows\$(1)
endef

setup: windows
	$(call build_setup,OpenDesk-Setup.exe)

setup-admin: windows-admin
	$(call build_setup,OpenDesk-Admin-Setup.exe)

setup-host: windows-host
	$(call build_setup,OpenDesk-Host-Setup.exe)

# ---- Relay ---------------------------------------------------------------------------------

relay:
	@if not exist builds\relay mkdir builds\relay
	cd server-relay && go build -trimpath -ldflags="-s -w -X opendesk/server-relay/internal/version.Version=$(VERSION)" -o ..\builds\relay\opendesk-relay.exe ./cmd/relay

run-relay:
	cd server-relay && go run ./cmd/relay -config config.json

docker-build:
	docker build --build-arg VERSION=$(VERSION) -t opendesk-relay:$(VERSION) server-relay

docker-up:
	docker compose up -d

docker-down:
	docker compose down

# ---- Android -------------------------------------------------------------------------------

android:
	cd client-android && gradlew.bat assembleDebug
	@if not exist builds\android mkdir builds\android
	copy /Y client-android\app\build\outputs\apk\debug\app-debug.apk builds\android\OpenDesk.apk

# ---- Quality -------------------------------------------------------------------------------

test:
	cd core-engine && cargo test --release
	cd client-desktop && cargo test --release
	cd server-relay && go test -race ./...
	cd client-android && gradlew.bat testDebugUnitTest

lint:
	cd core-engine && cargo fmt --check && cargo clippy --release --all-targets -- -D warnings
	cd client-desktop && cargo fmt --check && cargo clippy --release --all-targets -- -D warnings
	cd server-relay && gofmt -l . && go vet ./...

clean:
	cd core-engine && cargo clean
	cd client-desktop && cargo clean
	cd client-android && gradlew.bat clean
	@if exist builds rmdir /S /Q builds

else
# ---- Linux (and other POSIX systems) ------------------------------------------------------------
# Prerequisites: CONTRIBUTING.md#linux. Packages need `cargo install cargo-deb`.

SHELL := /bin/sh
DESKTOP := client-desktop
LINUX_OUT := builds/linux
# Honours CARGO_TARGET_DIR (absolute) like cargo does.
LINUX_BIN := $(or $(CARGO_TARGET_DIR),$(DESKTOP)/target)/release/opendesk-client

.PHONY: help all linux linux-admin linux-host linux-deb linux-tar relay run-relay android \
	docker-build docker-up docker-down test lint clean

help:
	@echo "OpenDesk $(VERSION)"
	@echo ""
	@echo "  make linux          Desktop client, standard edition   -> $(LINUX_OUT)/opendesk"
	@echo "  make linux-admin    Desktop client, admin edition      -> $(LINUX_OUT)/opendesk-admin"
	@echo "  make linux-host     Desktop client, headless host      -> $(LINUX_OUT)/opendesk-host"
	@echo "  make linux-deb      Debian/Ubuntu packages, all editions -> $(LINUX_OUT)/*.deb (needs cargo-deb)"
	@echo "  make linux-tar      Portable archive of the standard edition -> $(LINUX_OUT)/opendesk-$(VERSION)-linux-x86_64.tar.gz"
	@echo "  make relay          Relay server binary                -> builds/relay/opendesk-relay"
	@echo "  make run-relay      Run the relay from source (needs PostgreSQL, see server-relay/README.md)"
	@echo "  make android        Android debug APK                  -> builds/android/OpenDesk.apk"
	@echo "  make docker-build   Relay Docker image opendesk-relay:$(VERSION)"
	@echo "  make docker-up      Start the relay with Docker Compose"
	@echo "  make docker-down    Stop it"
	@echo "  make test           All unit tests"
	@echo "  make lint           Formatters and linters (clippy, go vet, gofmt)"
	@echo "  make clean          Remove build output"

all: relay linux

# ---- Desktop -------------------------------------------------------------------------------

# $(1) = Cargo features ("" for the standard edition), $(2) = output file name.
define build_linux
	cd $(DESKTOP) && cargo build --release --locked $(1)
	mkdir -p $(LINUX_OUT)
	cp $(LINUX_BIN) $(LINUX_OUT)/$(2)
endef

linux:
	$(call build_linux,,opendesk)

linux-admin:
	$(call build_linux,--features admin,opendesk-admin)

linux-host:
	$(call build_linux,--features headless,opendesk-host)

# cargo-deb builds each edition itself (features per variant in client-desktop/Cargo.toml).
linux-deb:
	mkdir -p $(LINUX_OUT)
	cd $(DESKTOP) && cargo deb --locked --output ../$(LINUX_OUT)/
	cd $(DESKTOP) && cargo deb --locked --variant admin --output ../$(LINUX_OUT)/
	cd $(DESKTOP) && cargo deb --locked --variant host --output ../$(LINUX_OUT)/

linux-tar: linux
	rm -rf $(LINUX_OUT)/opendesk-$(VERSION)
	mkdir -p $(LINUX_OUT)/opendesk-$(VERSION)
	cp $(LINUX_OUT)/opendesk $(DESKTOP)/icon.png packaging/linux/opendesk.desktop packaging/linux/install-openh264.sh \
		LICENSE $(LINUX_OUT)/opendesk-$(VERSION)/
	strip $(LINUX_OUT)/opendesk-$(VERSION)/opendesk
	tar -C $(LINUX_OUT) -czf $(LINUX_OUT)/opendesk-$(VERSION)-linux-$(shell uname -m).tar.gz opendesk-$(VERSION)
	rm -rf $(LINUX_OUT)/opendesk-$(VERSION)

# ---- Relay ---------------------------------------------------------------------------------

relay:
	mkdir -p builds/relay
	cd server-relay && go build -trimpath -ldflags="-s -w -X opendesk/server-relay/internal/version.Version=$(VERSION)" -o ../builds/relay/opendesk-relay ./cmd/relay

run-relay:
	cd server-relay && go run ./cmd/relay -config config.json

docker-build:
	docker build --build-arg VERSION=$(VERSION) -t opendesk-relay:$(VERSION) server-relay

docker-up:
	docker compose up -d

docker-down:
	docker compose down

# ---- Android -------------------------------------------------------------------------------

android:
	cd client-android && ./gradlew assembleDebug
	mkdir -p builds/android
	cp client-android/app/build/outputs/apk/debug/app-debug.apk builds/android/OpenDesk.apk

# ---- Quality -------------------------------------------------------------------------------

test:
	cd core-engine && cargo test --release
	cd $(DESKTOP) && cargo test --release
	cd server-relay && go test -race ./...
	cd client-android && ./gradlew testDebugUnitTest

lint:
	cd core-engine && cargo fmt --check && cargo clippy --release --all-targets -- -D warnings
	cd $(DESKTOP) && cargo fmt --check && cargo clippy --release --all-targets -- -D warnings
	cd server-relay && test -z "$$(gofmt -l .)" && go vet ./...

clean:
	cd core-engine && cargo clean
	cd $(DESKTOP) && cargo clean
	cd client-android && ./gradlew clean
	rm -rf builds
endif
