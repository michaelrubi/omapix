PREFIX ?= $(HOME)/.local
DARKTABLE ?= $(HOME)/.config/darktable
LUARC_LINE = require "omapix" -- edit in Omapix (installed by Omapix)

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo test

# Installs for the current user: binary, launcher entry, icon, and
# darktable's "edit in Omapix". Does not change which app opens images by
# default.
install: build
	install -Dm755 target/release/omapix $(PREFIX)/bin/omapix
	install -Dm644 assets/omapix.desktop $(PREFIX)/share/applications/omapix.desktop
	install -Dm644 assets/omapix.svg $(PREFIX)/share/icons/hicolor/scalable/apps/omapix.svg
	-update-desktop-database $(PREFIX)/share/applications 2>/dev/null
	@# "edit in Omapix" in darktable, if darktable is set up.
	@if [ -d $(DARKTABLE) ]; then \
		install -Dm644 assets/darktable/omapix.lua $(DARKTABLE)/lua/omapix.lua; \
		grep -qxF '$(LUARC_LINE)' $(DARKTABLE)/luarc 2>/dev/null || echo '$(LUARC_LINE)' >> $(DARKTABLE)/luarc; \
		echo "darktable: \"edit in Omapix\" installed (restart darktable)"; \
	fi

uninstall:
	rm -f $(PREFIX)/bin/omapix $(PREFIX)/share/applications/omapix.desktop \
		$(PREFIX)/share/icons/hicolor/scalable/apps/omapix.svg $(DARKTABLE)/lua/omapix.lua
	-sed -i '\|^require "omapix"|d' $(DARKTABLE)/luarc 2>/dev/null
