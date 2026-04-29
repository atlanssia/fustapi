# FustAPI Makefile
# Local-first, high-performance LLM API aggregation gateway

BINARY    := fustapi
TARGET    := target/release/$(BINARY)
INSTALL_DIR ?= /usr/local/bin

.PHONY: all build test clippy run clean install package help

all: clippy test

## build: Compile release binary
build:
	cargo build --release

## test: Run all tests
test:
	cargo test

## clippy: Run clippy with no warnings allowed
clippy:
	cargo clippy -- -D warnings

## check: Run cargo check (fast compilation check)
check:
	cargo check

## run: Run the server in dev mode
run:
	cargo run -- serve

## run-release: Run the release server
run-release: build
	$(TARGET) serve

## format: Format code with rustfmt
format:
	cargo fmt

## format-check: Check code formatting without modifying
format-check:
	cargo fmt --check

## lint: Run clippy + format check
lint: clippy format-check

## package: Build release binary and create tarball
package: build
	tar -czf $(BINARY)-$(shell cargo metadata --quiet --format-version=1 | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])").tar.gz -C target/release $(BINARY)

## install: Install binary to $(INSTALL_DIR) (requires sudo)
install: build
	install -m 755 $(TARGET) $(INSTALL_DIR)/$(BINARY)

## uninstall: Remove binary from $(INSTALL_DIR)
uninstall:
	rm -f $(INSTALL_DIR)/$(BINARY)

## clean: Remove build artifacts
clean:
	cargo clean

## help: Show this help message
help:
	@echo "FustAPI Makefile targets:"
	@echo ""
	@echo "  build        - Compile release binary"
	@echo "  test         - Run all tests"
	@echo "  clippy       - Run clippy (fails on warnings)"
	@echo "  check        - Fast compilation check"
	@echo "  run          - Run server in dev mode"
	@echo "  run-release  - Build and run release server"
	@echo "  format       - Format code with rustfmt"
	@echo "  format-check - Check formatting without modifying"
	@echo "  lint         - Run clippy + format check"
	@echo "  package      - Build release and create tarball"
	@echo "  install      - Install binary to $(INSTALL_DIR)"
	@echo "  uninstall    - Remove binary from $(INSTALL_DIR)"
	@echo "  clean        - Remove build artifacts"
	@echo "  help         - Show this help message"
