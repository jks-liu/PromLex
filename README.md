# PromLex · 语锚

PromLex is a Tauri-based AI translation studio for text and documents. The desktop app starts a local Web service, so the same interface and features are available in the installed app and at `http://127.0.0.1:48731`.

## Features

- Text translation with reusable prompt templates and any number of selected glossaries.
- Preset or custom source and target languages, including a one-click language swap.
- Persistent translation history shared by the desktop app and local Web interface.
- Import and export of prompts and glossaries as JSON.
- AI providers use the OpenAI Responses API shape. Cloud services and local runtimes such as Ollama, llama.cpp and LM Studio can be configured by entering their OpenAI-compatible base URL.
- Provider models define their context size, maximum segments per batch, maximum concurrent requests and whether the model is reserved for text translation.
- HTTP, HTTPS, SOCKS5 and SOCKS5H proxy support.
- English and Simplified Chinese interfaces.
- Streaming text and file translation with selectable reasoning effort, optional document summaries, cancellation and queued file jobs.
- Document translation for PDF, DOCX, PPTX, XLSX, TXT, Markdown, HTML, CSV, JSON, SRT and VTT.
- Optional system and AI conversation logs with configurable minimum log level.
- Image translation for PNG, JPG and WebP, including images embedded in Office documents, when the configured Responses API provider supports image input and the `image_generation` tool.
- Local-only Web access with the same React interface and Rust API used by the desktop app.

Office Open XML documents are unpacked locally. PromLex extracts paragraph or text-block content, translates it, replaces the original text, and writes a new document while retaining the package structure and formatting data. Images are left unchanged unless image support is explicitly enabled for the selected provider.

## Build and Run

### Prerequisites

- Node.js 20 or newer and npm.
- Rust 1.77 or newer with Cargo.
- The platform prerequisites for Tauri 2. On Windows, install the WebView2 Runtime and Visual Studio Build Tools with the Desktop C++ workload and Windows SDK.
- A configured AI provider is required before a translation can be performed.

Check the toolchain before installing dependencies:

```powershell
node --version
npm --version
rustc --version
cargo --version
```

### Install

Run these commands from the repository root:

```powershell
npm ci
```

Use `npm install` instead when `package-lock.json` is intentionally being updated.

### Desktop development

Start the Tauri desktop application with:

```powershell
npm run tauri dev
```

This starts the Vite frontend at `http://localhost:1420` and the local API service at `http://127.0.0.1:48731`. Do not add `--server` to this command; `--server` is for the standalone Web service and does not open a desktop window.

### Frontend and standalone Web development

To run the Web interface without a Tauri window, build the frontend and start the Rust server separately:

```powershell
npm run build
cargo build --manifest-path src-tauri/Cargo.toml
src-tauri\target\debug\promlex.exe --server
```

Open `http://127.0.0.1:48731` in a browser. On Linux or macOS, use `src-tauri/target/debug/promlex --server` instead. The standalone server is intended for Web access and automated tests; normal desktop users should launch the Tauri application.

### Release builds

Build the frontend and an unbundled optimized desktop executable:

```powershell
npm run tauri build -- --no-bundle
```

The executable is written to `src-tauri/target/release/promlex.exe` on Windows (or `src-tauri/target/release/promlex` on Linux/macOS). This is useful for a quick local smoke test, but it does not create an installer.

To create the platform installer and bundled application:

```powershell
npm run tauri build
```

Artifacts are placed under `src-tauri/target/release/bundle/`. The exact subdirectory and file type depend on the host operating system and the configured Tauri bundle targets.

### Versioning

Use the version bump script so npm, Cargo and Tauri metadata stay synchronized:

```powershell
# 0.1.1 -> 0.1.2 (default)
npm run version:bump

# Bump and build the Tauri installer/application
npm run version:bump -- patch --build

# Or choose the release level explicitly
npm run version:bump -- minor
npm run version:bump -- major

# Or set an exact version
npm run version:bump -- 1.0.0

# Explicitly skip the build (the default behavior)
npm run version:bump -- patch --no-build

# Show the current synchronized version without changing files
npm run version:bump -- current
```

The script updates `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json`. It stops before writing if any of those files contain a different current version.
By default it only updates version metadata. Add `--build` to run `npm run tauri build` after the update; use `--no-build` to make the choice explicit.

### Automatic GitHub releases

See [the automatic release guide (中文)](RELEASE.md) for setup, release commands, build artifacts and failure recovery.

Push a tag named `releases/vX.Y.Z` to trigger `.github/workflows/release.yml`. The tag must point to a commit containing this workflow, and its version must match all npm, Cargo and Tauri version metadata. Prerelease tags and versions with leading zeroes are rejected.

For example, to release version 1.0.2:

```powershell
npm run version:bump -- 1.0.2
node scripts/check-release-version.mjs releases/v1.0.2
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "Release 1.0.2"
git push origin HEAD
git tag -a releases/v1.0.2 -m "Release 1.0.2"
git push origin releases/v1.0.2
```

The workflow runs frontend unit tests, Rust formatting, Clippy and Rust unit tests before packaging. It builds Windows x64 NSIS/MSI installers, Linux x64 DEB/RPM/AppImage packages, and macOS Apple Silicon and Intel DMG installers. Tauri's build step also type-checks and builds the frontend.

Only after every build succeeds does a final job create a draft Release, upload all packages and publish it with generated release notes. Build failures leave no new Release. If uploading fails, the Release stays a draft; rerun the failed job from the Actions tab to finish uploading and publishing. An already published Release is never overwritten by a rerun.

GitHub Actions must be enabled and repository/organization policy must allow the workflow's `contents: write` permission. The workflow uses GitHub's automatic `GITHUB_TOKEN`; no personal access token is needed. Packages currently have no configured Windows signing or macOS signing/notarization; signing requires a separate certificate and secrets setup.

### Verification

Run the focused checks before packaging:

```powershell
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

The end-to-end suite starts its own isolated Web service and checks desktop, mobile and Simplified Chinese layouts:

```powershell
npx playwright install chromium
npm run test:e2e
```

The browser installation command is only needed once per machine. If Chromium is unavailable, set `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to a compatible local Chromium or Edge executable.

## Configuration and data

Configure at least one enabled AI provider before translating. Use a base URL that exposes the Responses API and enter the model's context size manually when model discovery does not report it. Provider API keys and all application data are stored in the current user's platform data directory and are never sent to the PromLex Web frontend except through the local loopback service. Web binding is restricted to loopback addresses (`127.0.0.1` or `::1`) so another device cannot read the local configuration.

Changes to the Web listening address or port take effect after restarting PromLex. The default prompt can be edited but is retained if deletion is requested, ensuring a usable translation instruction is always available.

## Troubleshooting

The desktop app and standalone Web server use port `48731` by default. Only one process can listen on that port:

```powershell
Get-NetTCPConnection -LocalPort 48731 -ErrorAction SilentlyContinue
```

Close an existing PromLex instance before starting another standalone server or running the end-to-end suite. The Web address and port can also be changed in Settings; restart PromLex after changing them.

For isolated tests or development data, set `TRANOVA_DATA_DIR` to a separate directory before starting the server. This prevents test providers, prompts, glossaries and history from changing your normal application data.

## Platform notes

Tauri desktop packaging is configured for Windows, Linux and macOS. Mobile packaging is not enabled in this release because the current architecture intentionally exposes no Web service on mobile and would require a separate Tauri command transport for feature parity.

The Chinese product name is **语锚**. PromLex retains the previous Tranova data directory and browser preference keys so existing settings and history remain available. `PROMLEX_DATA_DIR` takes precedence over the compatible `TRANOVA_DATA_DIR` override.

Tauri caches absolute paths to generated plugin permissions. The build script relocates these cached paths when the repository (including `target/`) is moved or renamed; no manual cache cleanup is required.
