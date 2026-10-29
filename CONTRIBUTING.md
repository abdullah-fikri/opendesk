# Contributing to OpenDesk

Read [docs/architecture.md](docs/architecture.md) first: it explains the components and the layering every
change must respect.

Before your first pull request is merged, sign the [Contributor License Agreement](CLA.md): the CLA bot asks you
to comment `I have read the CLA Document and I hereby sign the CLA` on the pull request. You keep the copyright;
the agreement lets the project offer OpenDesk under both the AGPL and commercial licenses.

Found a security problem? Do not open an issue: follow [SECURITY.md](SECURITY.md).

## Prerequisites

| Component | Tools |
| --- | --- |
| Desktop client, core engine | Rust stable 1.92+ (edition 2024) on Windows 10/11 or Linux (see below) |
| Relay | Go 1.26, PostgreSQL 13+ (or Docker) |
| Android | JDK 17+, Android SDK platform 36 |
| Windows installer | Inno Setup 6 |
| Linux packages | `cargo install cargo-deb` |
| Everything via `make` | GNU Make (the Makefile uses `cmd.exe` on Windows and `sh` elsewhere) |

### Windows

Rust (MSVC or GNU toolchain). Optional: `windres` (MinGW) to embed the icon and version resource.

### Linux

Debian/Ubuntu packages for building and testing the desktop client:

```bash
sudo apt install build-essential pkg-config libssl-dev libasound2-dev libfontconfig1-dev libxkbcommon-dev
# Running it needs an X11 or XWayland session and these libraries, which the UI toolkit loads at runtime:
sudo apt install libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon-x11-0 libgl1
# Live tests without a desktop (as CI runs them):
sudo apt install xvfb xauth dbus-x11 bzip2
```

Fedora: `dnf install gcc pkgconf openssl-devel alsa-lib-devel fontconfig-devel libxkbcommon-devel`. Hosting needs an
X11 session; under Wayland the client is a viewer only ([docs/platforms.md](docs/platforms.md)). H.264 needs
Cisco's OpenH264: `packaging/linux/install-openh264.sh`.

## Everyday commands

```bash
make help          # every target
make test          # all unit tests
make lint          # rustfmt, clippy, gofmt, go vet
make windows       # Windows: desktop client -> builds\windows\OpenDesk.exe
make linux         # Linux: desktop client -> builds/linux/opendesk (linux-deb, linux-tar for packages)
make run-relay     # relay from source on :8088
make android       # debug APK
```

Per component: see the README in `client-desktop/`, `server-relay/`, `client-android/` and `core-engine/`.

## Architecture rules

1. **Respect the layers.** Entry points and presentation call use cases; use cases call domain code and the
   interfaces they declare; adapters implement those interfaces. Never import upward (a service must not touch
   Slint widgets, a Go usecase must not import `handler` or `postgres`).
2. **DTOs are data.** Wire types live in `protocol/` (Rust), `internal/dto` (Go) and `data/dto` (Kotlin), with no
   logic. Validate and decide in the use-case layer.
3. **Domain code performs no I/O** and gets unit tests.
4. **No dead code.** Remove what nothing calls; do not keep commented-out code or "legacy" branches without a
   live producer. Compatibility paths stay only while a shipped client still uses them, and say which one.
5. **Protocol changes start in [docs/protocol.md](docs/protocol.md)** and follow its compatibility rules
   (new behavior behind a capability, old message shapes unchanged).

## Code conventions

**Language.** Everything in the code is English: identifiers, comments, log lines, error messages and user-facing
strings. Android user-facing strings live in `res/values/strings.xml` with translations in `values-<lang>/`.

**File and identifier naming.** Follow each language's convention:

| Language | Files | Types | Functions and variables | Constants |
| --- | --- | --- | --- | --- |
| Rust | `snake_case.rs`, `mod.rs` only for modules with submodules | `PascalCase` | `snake_case` | `SCREAMING_SNAKE_CASE` |
| Go | `snake_case.go`, package names short lowercase | `PascalCase` (exported), `camelCase` | `camelCase` | `camelCase` / `PascalCase` |
| Kotlin | `PascalCase.kt`, one top-level class per file | `PascalCase` | `camelCase` | `SCREAMING_SNAKE_CASE` |
| Slint | `snake_case.slint`, one component per file | `PascalCase` | `snake_case` properties and callbacks | |
| Android resources, SQL migrations | `snake_case`, migrations `000N_description.sql` | | | |
| Markdown docs | `kebab-case.md` | | | |

**Comments and docs.**

- Every module/file starts with a short comment saying what it is for.
- Every public function, type and constant has a doc comment saying what it does and anything a caller must
  know (units, threads, locks it needs, what it returns on failure).
- Mark code that needs care: `SECURITY:` for anything that validates data from a remote peer, `SAFETY:` above
  every Rust `unsafe` block that is not obvious, and a plain comment for concurrency or protocol subtleties.
- Explain *why*, not *what*. Do not narrate the change history in comments; that belongs in commits.
- Every feature has a page under its component's `docs/features/`. Update it in the same pull request.

**Formatting and linting.** CI rejects pull requests that are not clean:

```bash
cargo fmt && cargo clippy --release --all-targets -- -D warnings     # in core-engine/ and client-desktop/
gofmt -w . && go vet ./...                                          # in server-relay/
./gradlew lintDebug                                                 # in client-android/
```

## Tests

- Domain and use-case code gets unit tests next to the code (`#[cfg(test)] mod tests`, `*_test.go`,
  `src/test/...`).
- Tests that need a desktop session, a clipboard owner or a running relay are `#[ignore]`d and say how to run
  them. `make e2e` (`scripts/e2e.sh`) runs every end-to-end test against a throwaway relay and PostgreSQL
  (Docker); new relay or client flows that cross the network belong in `server-relay/e2e/` or `relay_e2e.rs`. On Linux the live
  X11 tests run headless: `xvfb-run -a cargo test --release -- --include-ignored` in `core-engine/`.
- Platform code: keep one public API per module and add a backend per OS (see
  [docs/architecture.md](docs/architecture.md#platform-code)). Run clippy for Linux too (CI does); from Windows,
  Docker with the packages above is the quickest way.

## Commits and pull requests

Use [Conventional Commits](https://www.conventionalcommits.org) with the component as scope:

```text
feat(desktop): add audio device selector
fix(relay): reject file chunks for an expired grant
docs(android): describe the trackpad gestures
```

A pull request explains the problem, the change, any protocol or storage impact, and how it was verified. Keep
the version in `VERSION` unchanged; releases bump it (see [docs/ci-cd.md](docs/ci-cd.md)).
