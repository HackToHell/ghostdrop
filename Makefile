APP       := Ghostdrop
BUILD     := build
APPDIR    := $(BUILD)/$(APP).app
BIN       := $(APPDIR)/Contents/MacOS/ghostdrop
LIBGHOSTTY:= $(BUILD)/ghostty/lib/libghostty.a
MODULEDIR := $(BUILD)/include/GhosttyKit
SOURCES   := $(wildcard Sources/Ghostdrop/*.swift)
TARGET    ?= arm64-apple-macos13.0
PREFIX    ?= /Applications
VERSION   := $(shell /usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" Resources/Info.plist)
DMG       := $(BUILD)/$(APP)-$(VERSION).dmg
LSREGISTER:= /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

# Ad-hoc by default. For distribution: SIGN_IDENTITY="Developer ID Application: …"
SIGN_IDENTITY ?= -
ifeq ($(SIGN_IDENTITY),-)
CODESIGN_FLAGS := --force --sign - --timestamp=none
else
CODESIGN_FLAGS := --force --sign "$(SIGN_IDENTITY)" --options runtime --timestamp
endif

FRAMEWORKS := AppKit Metal QuartzCore CoreText CoreGraphics CoreVideo IOSurface IOKit Carbon \
              UniformTypeIdentifiers UserNotifications ServiceManagement

SWIFTFLAGS := -O -wmo -swift-version 5 -target $(TARGET) -I $(MODULEDIR) \
              $(addprefix -framework ,$(FRAMEWORKS)) -lc++ -Xlinker -dead_strip

.PHONY: all app libghostty run install dmg notarize clean distclean pkg

all: app

libghostty: $(LIBGHOSTTY)

$(LIBGHOSTTY):
	./scripts/build-libghostty.sh

$(MODULEDIR)/module.modulemap: $(LIBGHOSTTY)
	mkdir -p $(MODULEDIR)
	cp $(BUILD)/ghostty/include/ghostty.h $(MODULEDIR)/ghostty.h
	printf 'module GhosttyKit {\n  header "ghostty.h"\n  export *\n}\n' > $@

app: $(BIN)

$(BIN): $(SOURCES) $(LIBGHOSTTY) $(MODULEDIR)/module.modulemap Resources/Info.plist Resources/AGENTS.md
	mkdir -p $(APPDIR)/Contents/MacOS $(APPDIR)/Contents/Resources
	swiftc $(SWIFTFLAGS) $(SOURCES) $(LIBGHOSTTY) -o $(BIN)
	cp Resources/Info.plist $(APPDIR)/Contents/Info.plist
	cp Resources/AGENTS.md $(APPDIR)/Contents/Resources/AGENTS.md
	rsync -a --delete $(BUILD)/ghostty/share/ghostty/ $(APPDIR)/Contents/Resources/ghostty/
	rsync -a --delete $(BUILD)/ghostty/share/terminfo/ $(APPDIR)/Contents/Resources/terminfo/
	codesign $(CODESIGN_FLAGS) $(APPDIR)
	@echo "==> built $(APPDIR) ($$(du -sh $(APPDIR) | cut -f1))"

run: app
	-pkill -x ghostdrop; sleep 0.3
	open $(APPDIR)

# Installs to a stable location and makes it the only registered copy, so
# macOS privacy rules (Local Network, notifications, login item) attach to it.
install: app
	-pkill -x ghostdrop; sleep 0.5
	rm -rf $(PREFIX)/$(APP).app
	ditto $(APPDIR) $(PREFIX)/$(APP).app
	-$(LSREGISTER) -u $(abspath $(APPDIR))
	$(LSREGISTER) -f $(PREFIX)/$(APP).app
	@echo "==> installed to $(PREFIX)/$(APP).app"

# Drag-to-install disk image: the app, an Applications shortcut, and the AI setup guide.
dmg: app
	rm -rf $(BUILD)/dmg $(DMG)
	mkdir -p $(BUILD)/dmg
	ditto $(APPDIR) $(BUILD)/dmg/$(APP).app
	ln -s /Applications $(BUILD)/dmg/Applications
	cp Resources/AGENTS.md "$(BUILD)/dmg/Set Up with AI.md"
	hdiutil create -quiet -volname "$(APP)" -srcfolder $(BUILD)/dmg -ov -format UDZO $(DMG)
	-$(LSREGISTER) -u $(abspath $(BUILD)/dmg/$(APP).app)
	rm -rf $(BUILD)/dmg
	if [ "$(SIGN_IDENTITY)" != "-" ]; then codesign --sign "$(SIGN_IDENTITY)" --timestamp $(DMG); fi
	@echo "==> $(DMG) ($$(du -h $(DMG) | cut -f1))"

# Requires a Developer ID signature and `xcrun notarytool store-credentials $(NOTARY_PROFILE)`.
NOTARY_PROFILE ?= ghostdrop
notarize: dmg
	xcrun notarytool submit $(DMG) --keychain-profile $(NOTARY_PROFILE) --wait
	xcrun stapler staple $(DMG)

# Linux (Arch): build and install the package from this checkout (makepkg -si).
pkg:
	cd packaging/arch && makepkg -si

clean:
	rm -rf $(APPDIR) $(BUILD)/include

distclean:
	rm -rf $(BUILD)
