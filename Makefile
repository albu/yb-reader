# YB Reader — build & deploy helper.
# The LD/AR overrides exist because MuPDF's make uses `ld -r -b binary` and
# `ar` to embed its fonts: Apple's tools silently drop ELF objects. lld +
# llvm-ar (via Homebrew) handle them. See README "Cross-compiling".
#
# The Kindle PW5 runs a 32-bit kernel (uname: armv7l), so the target is
# arm-unknown-linux-musleabihf, not aarch64.

TARGET  ?= arm-unknown-linux-musleabihf
LLD     ?= /opt/homebrew/opt/lld@21/bin/ld.lld
LLVM_AR ?= /opt/homebrew/opt/llvm@22/bin/llvm-ar

export LD := $(LLD) -m armelf_linux_eabi
export AR := $(LLVM_AR)

.PHONY: all setup check build probe deploy deploy-usb deploy-probe clean

all: build

setup:
	rustup target add $(TARGET)
	brew install zig cargo-zigbuild lld llvm

check:
	cargo check --workspace

build:
	cargo zigbuild --target $(TARGET) --release

probe:
	cargo zigbuild --target $(TARGET) --release -p yb-probe

# deploy = SSH fast loop (see deploy.sh); deploy-usb for a mounted Kindle.
deploy: build
	chmod +x deploy.sh
	./deploy.sh

deploy-usb: build
	chmod +x deploy.sh
	./deploy.sh usb

deploy-probe: probe
	chmod +x deploy.sh
	./deploy.sh probe

clean:
	cargo clean
