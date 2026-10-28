# Lanes' tasks (CONVENTIONS.md, `task-runner`). Recipes run from the repo
# root, whichever directory `just` is called from.
#
# dev: the dev app (Lanes Switch) and the lanes-cli watcher, under
# process-compose (what runs: process-compose.yaml), in the foreground like
# `docker compose up`: follows their logs, and Ctrl+C stops them. In the
# background instead (as Claude does): `dp ps up -d`, then
# `dp ps logs lanes -f` from anywhere, `dp ps down -p lanes`.
#
# build: ~/.local/bin/lanes links to lanes-cli/target/debug/lanes, which
# `just dev`'s lanes-cli process keeps rebuilt on every change; this is the
# same build, once.
#
# build-claude-session: into claude-session/bin/, where
# ~/.local/bin/claude-session links (claude-session/README.md).
#
# install: Lanes Switch is a platform app that still installs; whether it
# becomes dev-only is undecided.

# Lanes Switch and the lanes-cli watcher, under process-compose
dev:
	dp ps up

# Every test: lanes-cli (library and CLI) and Lanes Switch's Rust side
test:
	cd lanes-cli && cargo test
	cd ui/src-tauri && cargo test

# Clippy on both crates, kept at zero warnings (CONVENTIONS.md, `rust`)
clippy:
	cd lanes-cli && cargo clippy --all-targets
	cd ui/src-tauri && cargo clippy --all-targets

# Build the `lanes` binary (what ~/.local/bin/lanes links to)
build:
	cd lanes-cli && cargo build

# Build claude-session (Go) into claude-session/bin/
build-claude-session:
	cd claude-session && go build -o bin/claude-session .

# Lanes Switch's release bundle, copied to /Applications
install:
	#!/usr/bin/env bash
	set -e
	cd ui
	npm run tauri build
	APP="src-tauri/target/release/bundle/macos/Lanes Switch.app"
	rm -rf "/Applications/Lanes Switch.app"
	cp -R "$APP" /Applications/
