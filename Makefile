# Plain installation for distributions with a normal library path.
#
# On NixOS use the flake instead (`nix profile install .`): a bare binary
# cannot find the libraries winit and wgpu dlopen at run time there.

PREFIX ?= /usr/local
DESTDIR ?=
CARGO ?= cargo

BIN := target/release/pulpit
# Package version, parsed from the workspace manifest.
VERSION := $(shell awk -F'"' '/^version/ { print $$2; exit }' Cargo.toml)
HOST ?= 127.0.0.1
PORT ?= 8000

# The PDFium artifact the fetch script lays down, which is named for the
# platform it was fetched on. Everything downstream depends on this name
# rather than assuming an ELF one.
UNAME_S := $(shell uname -s)
PDFIUM_LIB := lib/libpdfium.so
ifeq ($(UNAME_S),Darwin)
PDFIUM_LIB := lib/libpdfium.dylib
endif
# Git Bash and MSYS report MINGW64_NT-… / MSYS_NT-…, so this matches a prefix.
ifneq (,$(filter MINGW% MSYS% CYGWIN%,$(UNAME_S)))
PDFIUM_LIB := lib/pdfium.dll
endif

.PHONY: help all build launch test check lint pdfium install uninstall bundle \
        linux-packages app app-universal icons windows website serve version \
        bump release clean sign-oracle-setup sign-oracle fuzz-sign

help:  ## Display this help screen
	@echo -e "\033[1mAvailable commands:\033[0m\n"
	@grep -E '^[a-z.A-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}' | sort

# ==============================================================================
# Build targets
# ==============================================================================

all: build

build:  ## Build the release binary
	$(CARGO) build --release

# Development launcher inside the dev shell, which supplies the NixOS runtime
# libraries and the PDFium path a cargo-built binary needs. `nix run` builds
# the complete immutable package instead; this uses Cargo's incremental cache.
# Every worker — the renderer and the browser that plays media — is a role of
# this one binary, re-executed with a flag, so there is nothing else to build.
launch:  ## Launch the app in the dev shell (optionally: make launch DECK=deck.pdf)
	nix develop . --command $(CARGO) run -p pulpit -- $(DECK)

# The same launcher, optimised. Any statement about how fast pulpit is has to
# come from this one: `launch` builds a debug binary, where PDFium is still
# the optimised library it ships as but every frame copy and every worker
# response around it is not. A debug session reads as a sluggish application
# and measures as one, which has already cost a long investigation.
launch-release: ## Launch the optimised build — the only one worth timing
	nix develop . --command $(CARGO) run --release -p pulpit -- $(DECK)

# A file target, so the download happens once and again only when the pin
# moves: the script carries the release tag and per-platform hashes.
$(PDFIUM_LIB): scripts/fetch-pdfium.sh
	./scripts/fetch-pdfium.sh

pdfium: $(PDFIUM_LIB)  ## Fetch the pinned, hash-verified PDFium into ./lib

# A relocatable directory that carries libpdfium and a launcher.
bundle: build $(PDFIUM_LIB)  ## Build a relocatable directory with libpdfium and a launcher
	./scripts/make-bundle.sh

# The primary Linux deliverable: a .deb and a .rpm generated from one
# description, declaring the dlopen set as dependencies and a Chromium-family
# browser as a recommendation. Needs nfpm, which the dev shell carries.
linux-packages: build $(PDFIUM_LIB)  ## Build the .deb and .rpm into dist/ (Linux only)
	./scripts/make-linux-packages.sh

# The macOS application bundle: ad-hoc signed, with libpdfium beside the
# executable where the PDFium search order already looks. macOS only.
app: build $(PDFIUM_LIB)  ## Build and ad-hoc sign dist/Pulpit.app (macOS only)
	./scripts/make-app-bundle.sh

# The released macOS artifact: one bundle that runs natively on both Apple
# Silicon and Intel. Either architecture can build it — the Apple toolchain
# cross-compiles between them with no extra linker — so this needs one runner
# and produces one disk image and one Homebrew cask.
#
# Both PDFium artifacts are fetched, because `lipo` fuses libpdfium too: it is
# dlopen'd by explicit path, so a thin library inside a fat app is a run-time
# failure on the other architecture rather than a build error here.
app-universal: $(PDFIUM_LIB)  ## Build the universal dist/Pulpit.app (macOS only)
	rustup target add aarch64-apple-darwin x86_64-apple-darwin
	./scripts/fetch-pdfium.sh --platform mac-x64 --dest lib/mac-x64
	$(CARGO) build --release -p pulpit --target aarch64-apple-darwin
	$(CARGO) build --release -p pulpit --target x86_64-apple-darwin
	./scripts/make-app-bundle.sh --universal

# The Windows artifacts: the portable zip Scoop installs from, and the
# per-user installer winget runs. Windows only.
windows: build $(PDFIUM_LIB)  ## Build the Windows zip and per-user installer (Windows only)
	./scripts/make-windows-package.sh

# Regenerate the checked-in icon formats after changing packaging/pulpit.svg.
# Needs resvg and icotool, so it is not a dependency of any build.
icons:  ## Rebuild the .iconset and .ico from the SVG
	./scripts/make-icons.sh

# ==============================================================================
# Test targets
# ==============================================================================

test:  ## Run the whole test suite; no display required
	$(CARGO) test --workspace --no-fail-fast

check:  ## Run cargo check (fast compile check)
	$(CARGO) check --workspace --all-targets

lint:  ## Check formatting and run clippy with warnings denied
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

# ==============================================================================
# Signing feature test oracle targets
# ==============================================================================

sign-oracle-setup:  ## Set up venv and install sign-oracle dependencies
	@if command -v uv >/dev/null 2>&1; then \
		uv venv .venv-sign-oracle; \
		uv pip install -p .venv-sign-oracle/bin/python -r tools/sign-oracle/requirements.txt; \
	else \
		python3 -m venv .venv-sign-oracle; \
		. .venv-sign-oracle/bin/activate && pip install -q -r tools/sign-oracle/requirements.txt; \
	fi
	@echo "Sign oracle environment ready. Activate with: source .venv-sign-oracle/bin/activate"

sign-oracle:  ## Validate all signed PDF fixtures with pyHanko CLI
	@mkdir -p tools/sign-oracle/fixtures
	@. .venv-sign-oracle/bin/activate && bash tools/sign-oracle/verify-fixtures.sh tools/sign-oracle/fixtures

fuzz-sign:  ## Run all fuzzing targets for 60 seconds each (development fuzzing)
	@echo "Running fuzzing targets for PDF signature verification..."
	cd crates/pulpit-render && \
	cargo fuzz run fuzz_revision_map -- -max_total_time=60 && \
	cargo fuzz run fuzz_discover -- -max_total_time=60 && \
	cargo fuzz run fuzz_verify_full -- -max_total_time=60 && \
	cargo fuzz run fuzz_cms -- -max_total_time=60
	@echo "Fuzzing complete. For longer runs, see crates/pulpit-render/fuzz/README.md"

# ==============================================================================
# Documentation targets
# ==============================================================================

website:  ## Compile docs-src/ into docs/ with Calepin
	calepin compile docs-src docs

serve: website  ## Build and serve the website at http://$(HOST):$(PORT)
	calepin serve docs --host $(HOST) --port $(PORT) --open

# ==============================================================================
# Installation targets
# ==============================================================================

# Deliberately *not* `install: build`. A system-wide install needs root, and
# rebuilding under sudo leaves a root-owned target/ that every later ordinary
# build then fails on. Build as yourself, install as root.
#
#   make && sudo make install          # system-wide
#   make && make install PREFIX=~/.local   # just for you, no sudo
install:  ## Install the built binary and its data files under $(PREFIX)
	@test -x $(BIN) || { \
		echo "$(BIN) is missing — run 'make' first (as yourself, not root)."; \
		exit 1; \
	}
	@# On NixOS an unwrapped binary installs cleanly and then fails at startup
	@# with "could not start a graphical session", because winit and wgpu
	@# dlopen libraries that are not on any global loader path there. The flake
	@# wraps the binary with those paths; this cannot. Refusing is kinder than
	@# leaving a pulpit on $$PATH that never starts.
	@test ! -f /etc/NIXOS -o -n "$(FORCE_NIXOS_INSTALL)" || { \
		echo "This is NixOS, where a plain install produces a binary that cannot start."; \
		echo "Use the flake, which wraps it with the libraries it dlopens:"; \
		echo "    nix profile install ."; \
		echo "For a development run without installing:"; \
		echo "    make launch [DECK=deck.pdf]"; \
		echo "To install anyway (you will need to set LD_LIBRARY_PATH yourself):"; \
		echo "    make install FORCE_NIXOS_INSTALL=1"; \
		exit 1; \
	}
	@test -w $(DESTDIR)$(dir $(PREFIX)) -o -w $(DESTDIR)$(PREFIX) || { \
		echo "Cannot write to $(DESTDIR)$(PREFIX)."; \
		echo "Use 'sudo make install', or install for yourself with:"; \
		echo "    make install PREFIX=\$$HOME/.local"; \
		exit 1; \
	}
	install -Dm755 $(BIN) $(DESTDIR)$(PREFIX)/bin/pulpit
	install -Dm644 packaging/pulpit.desktop \
		$(DESTDIR)$(PREFIX)/share/applications/pulpit.desktop
	install -Dm644 packaging/pulpit.svg \
		$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/pulpit.svg
	install -Dm644 README.md $(DESTDIR)$(PREFIX)/share/doc/pulpit/README.md
	@# Other people's work travels with the binary, not just with the source.
	install -Dm644 LICENSES/README.md \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/README.md
	install -Dm644 LICENSES/LICENSE-MIT \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/LICENSE-MIT
	install -Dm644 LICENSES/LICENSE-APACHE \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/LICENSE-APACHE
	install -Dm644 LICENSES/ICED_AW-LICENSE \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/ICED_AW-LICENSE
	install -Dm644 LICENSES/LUCIDE-LICENSE \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/LUCIDE-LICENSE
	install -Dm644 LICENSES/TYPST_ASSETS-NOTICE \
		$(DESTDIR)$(PREFIX)/share/doc/pulpit/licenses/TYPST_ASSETS-NOTICE
	@if [ -f lib/libpdfium.so ]; then \
		install -Dm644 lib/libpdfium.so \
			$(DESTDIR)$(PREFIX)/lib/pulpit/libpdfium.so; \
		install -Dm644 lib/PDFIUM-LICENSE \
			$(DESTDIR)$(PREFIX)/share/doc/pulpit/PDFIUM-LICENSE 2>/dev/null || true; \
		echo "installed libpdfium into $(PREFIX)/lib/pulpit"; \
		echo "the binary finds it there by itself; no PULPIT_PDFIUM_PATH needed"; \
	else \
		echo "no lib/libpdfium.so — run 'make pdfium' first for real rendering"; \
	fi

uninstall:  ## Remove everything `make install` placed under $(PREFIX)
	rm -f $(DESTDIR)$(PREFIX)/bin/pulpit
	rm -f $(DESTDIR)$(PREFIX)/share/applications/pulpit.desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/pulpit.svg
	rm -rf $(DESTDIR)$(PREFIX)/share/doc/pulpit
	rm -rf $(DESTDIR)$(PREFIX)/lib/pulpit

# ==============================================================================
# Release targets
# ==============================================================================

version:  ## Print the current package version
	@echo $(VERSION)

# Bump the workspace version and refresh Cargo.lock. Usage:
# `make bump VERSION=0.2.0`.
bump:  ## Bump the workspace version (usage: make bump VERSION=x.y.z)
	@if [ -z "$(VERSION)" ] || [ "$(VERSION)" = "$(shell awk -F'"' '/^version/ { print $$2; exit }' Cargo.toml)" ]; then \
	    echo "usage: make bump VERSION=x.y.z  (must differ from current $(shell awk -F'"' '/^version/ { print $$2; exit }' Cargo.toml))"; \
	    exit 1; \
	fi
	@sed -i.bak -E '0,/^version = "[^"]*"/s//version = "$(VERSION)"/' Cargo.toml && rm Cargo.toml.bak
	@sed -i.bak -E '/^pulpit-/s/version = "[^"]*"/version = "$(VERSION)"/' Cargo.toml && rm Cargo.toml.bak
	@$(CARGO) update -w >/dev/null
	@echo "Bumped pulpit to $(VERSION)."
	@git diff --stat Cargo.toml Cargo.lock

# Tag the current commit and push the tag. This triggers:
#   - .github/workflows/release.yml        cargo-dist binaries and installers
#   - .github/workflows/publish-crates.yml cargo publish to crates.io
# Refuses to run on a dirty tree so the tag reflects committed code.
release:  ## Tag and push v$(VERSION); fires the release workflows
	@test -z "$$(git status --porcelain)" || { echo "working tree is dirty; commit or stash first"; exit 1; }
	@echo "Tagging v$(VERSION) at $$(git rev-parse --short HEAD) and pushing..."
	git tag -a v$(VERSION) -m "Release v$(VERSION)"
	git push origin v$(VERSION)

clean:  ## Remove build artifacts
	$(CARGO) clean
	rm -rf dist docs-src/.calepin docs/.calepin
