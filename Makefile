BIN_DIR  ?= $(HOME)/.local/bin
UNIT_DIR ?= $(HOME)/.config/systemd/user
BIN      := $(BIN_DIR)/cosmic-ws-isolate

.PHONY: build test install enable disable uninstall

build:
	cargo build --release

test:
	cargo test

# Installs the binary only; changes no COSMIC settings.
install: build
	install -Dm755 target/release/cosmic-ws-isolate $(BIN)
	install -Dm644 dist/cosmic-ws-isolate.service $(UNIT_DIR)/cosmic-ws-isolate.service
	systemctl --user daemon-reload

# Start the daemon now and at every login.
enable:
	systemctl --user enable --now cosmic-ws-isolate.service

disable:
	-systemctl --user disable --now cosmic-ws-isolate.service

# Puts every COSMIC setting back first, then removes the program.
uninstall:
	-$(BIN) restore --all
	-systemctl --user disable --now cosmic-ws-isolate.service
	rm -f $(BIN) $(UNIT_DIR)/cosmic-ws-isolate.service
	systemctl --user daemon-reload
