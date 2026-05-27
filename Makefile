# SKUA Makefile
# All commands assume you are in the repo root.

.PHONY: all build test test-sol test-rust fmt lint gas-report clean \
        deploy-testnet deploy-mainnet check-env

# ── Default ───────────────────────────────────────────────────
all: build test

# ── Rust bot ─────────────────────────────────────────────────
build:
	cd bot && cargo build --release

test-rust:
	cd bot && cargo test --all -- --nocapture

fmt-rust:
	cd bot && cargo fmt --all

lint-rust:
	cd bot && cargo clippy --all -- -D warnings

# ── Solidity contracts ────────────────────────────────────────
test-sol:
	cd contracts && forge test -vvvv

gas-report:
	cd contracts && forge test --gas-report

fmt-sol:
	cd contracts && forge fmt

build-sol:
	cd contracts && forge build

# ── Combined ──────────────────────────────────────────────────
test: test-rust test-sol

fmt: fmt-rust fmt-sol

# ── Environment check ─────────────────────────────────────────
# Verifies all required env vars are set before attempting deployment
check-env:
	@echo "Checking required environment variables..."
	@test -n "$$SKUA_CHAIN_ID"             || (echo "Missing: SKUA_CHAIN_ID" && exit 1)
	@test -n "$$SKUA_RPC_WS_URL"           || (echo "Missing: SKUA_RPC_WS_URL" && exit 1)
	@test -n "$$SKUA_RPC_HTTP_URL"         || (echo "Missing: SKUA_RPC_HTTP_URL" && exit 1)
	@test -n "$$SKUA_PROFIT_WALLET"        || (echo "Missing: SKUA_PROFIT_WALLET" && exit 1)
	@test -n "$$SKUA_BALANCER_VAULT"       || (echo "Missing: SKUA_BALANCER_VAULT" && exit 1)
	@test -n "$$SKUA_HYPERLEND_POOL"       || (echo "Missing: SKUA_HYPERLEND_POOL" && exit 1)
	@test -n "$$SKUA_BOT_KEY_1"            || (echo "Missing: SKUA_BOT_KEY_1" && exit 1)
	@test -n "$$SKUA_BOT_KEY_2"            || (echo "Missing: SKUA_BOT_KEY_2" && exit 1)
	@test -n "$$SKUA_BOT_KEY_3"            || (echo "Missing: SKUA_BOT_KEY_3" && exit 1)
	@test -n "$$SKUA_BOT_KEY_4"            || (echo "Missing: SKUA_BOT_KEY_4" && exit 1)
	@test -n "$$SKUA_BOT_KEY_5"            || (echo "Missing: SKUA_BOT_KEY_5" && exit 1)
	@test -n "$$SKUA_API_KEY"              || (echo "Missing: SKUA_API_KEY" && exit 1)
	@echo "All required env vars present."

# ── Deployment ────────────────────────────────────────────────
deploy-testnet: check-env build-sol
	cd contracts && forge script script/Deploy.s.sol:DeploySkua \
		--rpc-url $$SKUA_TESTNET_RPC \
		--broadcast \
		-vvvv

deploy-mainnet: check-env build-sol
	@echo "WARNING: Deploying to MAINNET. Press Ctrl+C to abort, Enter to continue."
	@read _confirm
	cd contracts && forge script script/Deploy.s.sol:DeploySkua \
		--rpc-url $$SKUA_RPC_HTTP_URL \
		--broadcast \
		--verify \
		-vvvv

# ── Run bot ───────────────────────────────────────────────────
run: check-env build
	cd bot && RUST_LOG=info ./target/release/skua

run-debug: check-env
	cd bot && RUST_LOG=debug cargo run --release

# ── Clean ─────────────────────────────────────────────────────
clean:
	cd bot && cargo clean
	cd contracts && forge clean
