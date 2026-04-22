.PHONY: dev dev-web build package install lint typecheck test test-rust test-ts check fmt clean clean-all

# Start Tauri app (frontend + Rust backend) in dev mode
dev:
	pnpm tauri dev

# Start frontend only (browser — Rust commands unavailable)
dev-web:
	pnpm dev

# Build production app
build:
	pnpm tauri build

# Package macOS app (.app + .dmg)
package:
	pnpm tauri build --bundles app,dmg
	@echo "\nPackaged:"
	@ls -lh src-tauri/target/release/bundle/dmg/*.dmg 2>/dev/null || true
	@ls -lh src-tauri/target/release/bundle/macos/*.app 2>/dev/null || true

# Install JS deps
install:
	pnpm install

# Lint frontend
lint:
	pnpm lint

# TS typecheck (no emit)
typecheck:
	pnpm exec tsc --noEmit

# Rust unit + integration tests
test-rust:
	cd src-tauri && cargo test

# Frontend typecheck (v0 has no JS tests yet)
test-ts: typecheck

# Everything
test: test-rust test-ts

# Rust compile-only
check:
	cd src-tauri && cargo check

# Rust format (rewrites files)
fmt:
	cd src-tauri && cargo fmt

# Clean dev artifacts
clean:
	rm -rf dist node_modules/.vite src-tauri/target/debug

# Clean everything including release builds
clean-all: clean
	rm -rf src-tauri/target/release
