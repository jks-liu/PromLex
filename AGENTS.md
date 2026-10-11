# Repository Guidelines

## Project Structure & Module Organization

PromLex combines a React/TypeScript interface with a Rust/Tauri service. Frontend code lives in `src/`: views are in `src/pages/`, UI in `src/components/`, and API, types, localization, and utilities are at the directory root. Rust code is under `src-tauri/src/`; `server.rs` exposes the loopback API, while `ai.rs`, `files.rs`, `jobs.rs`, `scheduler.rs`, and `store.rs` own backend domains. Desktop configuration and icons live in `src-tauri/`. Unit tests sit beside TypeScript modules as `*.test.ts`; Playwright tests and snapshots are in `tests/e2e/`.

## Product & Architecture Constraints

Keep the Tauri desktop app and local Web interface functionally equivalent.

## Build, Test, and Development Commands

- `npm ci`: install the locked Node dependencies.
- `npm run tauri dev`: run the desktop app with Vite hot reload.
- `npm run build`: type-check TypeScript and build the frontend.
- `npm test`: run frontend unit tests once with Vitest.
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust unit tests.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`: verify Rust formatting.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`: reject Rust warnings.
- `npm run test:e2e`: build and run Chromium end-to-end and screenshot tests. Install its browser once with `npx playwright install chromium`.

## Coding Style & Naming Conventions

TypeScript is strict and uses ES modules, two-space indentation, semicolons, and double quotes. Name React components and page files in PascalCase (`TranslationOptions.tsx`), functions and variables in camelCase, and tests after their subject. Keep API payload interfaces in `types.ts` and user-facing text in the i18n resources. Follow standard `rustfmt` output: four-space indentation, snake_case items, and PascalCase types.

Prefer the standard library. Add third-party dependencies only when mature, maintained, widely adopted, and replacing substantial code; avoid abandoned/niche packages or dependencies for trivial logic.

## Testing Guidelines

Keep E2E data isolated through the configured `TRANOVA_DATA_DIR`.

## Security & Configuration

Use a temporary `TRANOVA_DATA_DIR` when testing changes that write application state.

## Platform Scope

Target Windows, Linux, and macOS. Mobile packaging is optional, requires no local Web service, and is not enabled.
