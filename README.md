# SKUA — HyperEVM Flash Loan System

Production flash loan arbitrage bot for HyperEVM (Hyperliquid L1, Chain ID 999).

---

## Architecture

```
skua/
├── bot/                    # Rust workspace (async, multi-wallet, event-driven)
│   ├── crates/
│   │   ├── core/           # Config, BotState, gas constants, shared types
│   │   ├── chain/          # WS block loop, fee refresh, pool monitor
│   │   ├── hypercore/      # L1Read precompile, CoreWriter encoder, L2 order book WS
│   │   ├── simulation/     # AMM math (CP + StableSwap), GSS sizing, eth_call gate
│   │   ├── scanner/        # 5-layer pipeline, S1/S4 signal detectors
│   │   ├── executor/       # WalletPool (5+ wallets), eth_sendRawTransactionSync
│   │   ├── api/            # Axum REST, Prometheus /metrics, Telegram alerts
│   │   └── strategies/
│   │       ├── evm_arb/    # S1: EVM DEX ↔ HyperCore (two-phase, CoreWriter)
│   │       ├── liquidation/ # S2: HyperLend liquidations (fully atomic)
│   │       ├── triangular/ # S3: Cross-DEX triangular arb (fully atomic)
│   │       └── stable_depeg/ # S4: Stablecoin depeg (fully atomic)
│   └── src/main.rs         # Supervisor + strategy evaluation loop
├── contracts/              # Solidity (Foundry, Cancun, ^0.8.24)
│   ├── src/
│   │   ├── SkuaBase.sol         # Auth, profit guard, exact approvals
│   │   ├── LiquidationExecutor.sol
│   │   ├── TriArbExecutor.sol
│   │   ├── StableDepegExecutor.sol
│   │   └── EvmArbExecutor.sol  # + Escrow state machine
│   └── test/               # Foundry tests (10,000 fuzz runs)
└── dashboard/              # React 18 + TypeScript (separate spec)
```

---

## Six Production Laws

1. **Precompile values are block-scoped.** Never cache across blocks.
2. **CoreWriter delay is permanent.** ~3-5s. S1 only. Never in same tx as repay.
3. **3M gas is the fast-block ceiling.** All strategies target < 900K.
4. **gasPrice is the ordering game.** No Flashbots. 50×–500× multipliers at near-zero cost.
5. **Eight nonces per address.** Multi-wallet mandatory. Min 5, recommended 8–10.
6. **Optimal sizing is not optional.** 128-iteration golden-section search every time.

---

## Build Order (Non-Negotiable)

```
Phase 1: Foundation (Week 1-2)   — providers, state, fees, wallet pool, simulation
Phase 2: S2 Liquidations (Week 3-4) — most reliable, validate all infrastructure
Phase 3: S3 Triangular (Week 5-6)   — highest frequency, clean atomic execution
Phase 4: S4 Stable Depeg (Week 7)   — low frequency, high per-event profit
Phase 5: S1 EVM/Core Arb (Week 8-9) — highest alpha, most complex, deploy last
Phase 6: Scale + Dashboard (Week 10)
```

Never reverse this order. S2 validates the entire stack before CoreWriter complexity.

---

## Prerequisites

- Rust 1.78+ (`rustup update stable`)
- Foundry (`foundryup`)
- Private dedicated RPC (never the public endpoint)
- 5+ funded hot wallets (HYPE for gas only — never accumulate strategy tokens)
- Cold profit wallet (hardware wallet or MPC)

---

## Setup

```bash
# 1. Clone and set up environment
cp .env.example .env
# Fill in ALL values in .env — missing capital-related vars = hard startup failure

# 2. Run all tests
make test

# 3. Check gas profiles (all strategies must be < 2.5M gas)
make gas-report

# 4. Deploy to testnet (S2 first)
make deploy-testnet

# 5. 72h testnet run before mainnet
# 6. Deploy to mainnet
make deploy-mainnet
```

---

## Running the Bot

```bash
# Production
make run

# Debug logging
make run-debug
```

---

## API

```
GET  /health          → 200 OK
GET  /status          → JSON: block, base_fee, hype_price, kill_switch, strategy flags
GET  /metrics         → Prometheus text format
POST /kill            → {"api_key": "..."} — activate kill switch
POST /resume          → {"api_key": "..."} — clear kill switch + revert counter
POST /strategy        → {"api_key":"...","strategy":"s2","enabled":true}
```

---

## Circuit Breakers

Auto-halt conditions (checked before every submission):
- Manual kill switch via API
- `consecutive_reverts > SKUA_MAX_CONSECUTIVE_REVERTS` (default 3)
- `gas_spent_window > SKUA_GAS_KILL_HYPE` in `SKUA_GAS_KILL_WINDOW_SECS` (default 0.5 HYPE/hour)
- HYPE price precompile returns 0

All halt: bot stays running for monitoring, submissions blocked. Manual resume required.

---

## Security Checklist (Before Every Mainnet Deployment)

- [ ] Flash loan callbacks: `msg.sender == vault` AND `initiator == address(this)`
- [ ] All approvals: exact amount, reset to 0 after use — NO `type(uint256).max`
- [ ] `_assertProfitable()` present in every flash callback path
- [ ] `_safeOraclePx()` / `_safeSpotPx()` used — zero price causes revert
- [ ] Contract holds ZERO tokens between trades
- [ ] Gas profiled on testnet — all strategies < 3M gas
- [ ] 72h testnet run complete
- [ ] Private keys in env vars only — never in logs, never on disk
- [ ] Bot wallets hold HYPE for gas only — no strategy token accumulation
- [ ] Profit wallet is cold (hardware/MPC)
- [ ] Contract admin key is offline

---

## Key Contract Addresses (HyperEVM Mainnet)

| Contract | Address |
|---|---|
| L1Read Precompile | `0x0000000000000000000000000000000000000800` |
| CoreWriter | `0x3333333333333333333333333333333333333333` |
| All others | Set via environment after deployment |

---

## Monitoring

Prometheus metrics at `http://localhost:$SKUA_API_PORT/metrics`.

Key alerts:
- `skua_hype_price_usd == 0` → critical, all submissions blocked
- `skua_kill_switch_active == 1` → manual intervention required
- `skua_consecutive_reverts > 2` → circuit breaker imminent
- `skua_wallet_hype_balance{wallet_index="N"} < 0.05` → top up immediately
- No blocks in 30s → RPC connectivity issue
