PREFIX ?= $(HOME)/.local

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo test

# Installs for the current user: binary, launcher entry and icon. Does not
# change which app opens images by default.
install: build
	install -Dm755 target/release/omapix $(PREFIX)/bin/omapix
	install -Dm644 assets/omapix.desktop $(PREFIX)/share/applications/omapix.desktop
	install -Dm644 assets/omapix.svg $(PREFIX)/share/icons/hicolor/scalable/apps/omapix.svg
	-update-desktop-database $(PREFIX)/share/applications 2>/dev/null

uninstall:
	rm -f $(PREFIX)/bin/omapix $(PREFIX)/share/applications/omapix.desktop \
		$(PREFIX)/share/icons/hicolor/scalable/apps/omapix.svg
