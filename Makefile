APP       := Ghostdrop
BUILD     := build
APPDIR    := $(BUILD)/$(APP).app
BIN       := $(APPDIR)/Contents/MacOS/ghostdrop
LIBGHOSTTY:= $(BUILD)/ghostty/lib/libghostty.a
MODULEDIR := $(BUILD)/include/GhosttyKit
SOURCES   := $(wildcard Sources/Ghostdrop/*.swift)
TARGET    ?= arm64-apple-macos13.0
PREFIX    ?= /Applications

FRAMEWORKS := AppKit Metal QuartzCore CoreText CoreGraphics CoreVideo IOSurface IOKit Carbon \
              UniformTypeIdentifiers UserNotifications ServiceManagement

SWIFTFLAGS := -O -wmo -swift-version 5 -target $(TARGET) -I $(MODULEDIR) \
              $(addprefix -framework ,$(FRAMEWORKS)) -lc++ -Xlinker -dead_strip

.PHONY: all app libghostty run install clean distclean

all: app

libghostty: $(LIBGHOSTTY)

$(LIBGHOSTTY):
	./scripts/build-libghostty.sh

$(MODULEDIR)/module.modulemap: $(LIBGHOSTTY)
	mkdir -p $(MODULEDIR)
	cp $(BUILD)/ghostty/include/ghostty.h $(MODULEDIR)/ghostty.h
	printf 'module GhosttyKit {\n  header "ghostty.h"\n  export *\n}\n' > $@

app: $(BIN)

$(BIN): $(SOURCES) $(LIBGHOSTTY) $(MODULEDIR)/module.modulemap Resources/Info.plist
	mkdir -p $(APPDIR)/Contents/MacOS $(APPDIR)/Contents/Resources
	swiftc $(SWIFTFLAGS) $(SOURCES) $(LIBGHOSTTY) -o $(BIN)
	cp Resources/Info.plist $(APPDIR)/Contents/Info.plist
	rsync -a --delete $(BUILD)/ghostty/share/ghostty/ $(APPDIR)/Contents/Resources/ghostty/
	rsync -a --delete $(BUILD)/ghostty/share/terminfo/ $(APPDIR)/Contents/Resources/terminfo/
	codesign --force --sign - --timestamp=none $(APPDIR)
	@echo "==> built $(APPDIR) ($$(du -sh $(APPDIR) | cut -f1))"

run: app
	-pkill -x ghostdrop; sleep 0.3
	open $(APPDIR)

LSREGISTER := /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

# Installs to a stable location and makes it the only registered copy, so
# macOS privacy rules (Local Network, notifications, login item) attach to it.
install: app
	-pkill -x ghostdrop; sleep 0.5
	rm -rf $(PREFIX)/$(APP).app
	ditto $(APPDIR) $(PREFIX)/$(APP).app
	-$(LSREGISTER) -u $(abspath $(APPDIR))
	$(LSREGISTER) -f $(PREFIX)/$(APP).app
	@echo "==> installed to $(PREFIX)/$(APP).app"

clean:
	rm -rf $(APPDIR) $(BUILD)/include

distclean:
	rm -rf $(BUILD)
