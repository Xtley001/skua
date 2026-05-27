# SKUA — Production Readiness Audit Report
### Kingfisher Flash Loan Audit Protocol — HyperEVM Chain ID 999
### Date: 2026-05-26

---

## EXECUTIVE SUMMARY

**Total Findings: 23**

| Severity Class | Count |
|---|---|
| SECURITY | 4 |
| COMPLETENESS (production-blocking stubs) | 8 |
| CALCULATION | 3 |
| HARDCODING | 3 |
| ARCHITECTURE | 3 |
| CONFIG | 2 |

**Verdict: NOT PRODUCTION READY.** Eight findings are production-blocking stubs — code
that will compile and pass tests but will do nothing on a live chain. Four are security
issues. The system must address all SECURITY and COMPLETENESS findings before going live
with any capital.

---

## FINDINGS

---

```
=====================================
FINDING #1
=====================================
File(s):     bot/crates/strategies/liquidation/src/monitor.rs
Line(s):     L197–L209
Category:    COMPLETENESS
Section:     §1.2 TODO/Stub Scan
Description: refresh_borrower_position() — the function that populates
             BorrowerPosition with live collateral and debt data — is a
             no-op stub. Its entire body is a TODO comment and a
             tracing::debug! log. It returns Ok(()) without reading
             any on-chain state.

             This means BotState.hyperlend_positions is ALWAYS empty.
             S2 (liquidation) therefore never evaluates any position,
             never fires, and generates zero revenue. The strategy
             appears enabled but is completely inert.

Operator Law: Part 0 Absolute Ban — "// TODO: replace with real X" ships
              working or doesn't ship. SKILL.md §14.1.
Impact:       S2 is entirely non-functional. Zero liquidations will execute.
              Any gas burned on position-indexer infrastructure is wasted.
Fix:          Implement the full getUserReserveData loop for all registered
              HyperLend assets. For each asset, call
              IHyperLendPool.getUserReserveData(asset, borrower), decode
              currentATokenBalance (collateral) and
              currentStableDebt + currentVariableDebt (debt), populate
              CollateralAsset and DebtAsset structs, then recompute HF
              and store back to BotState. Must match the on-chain HF formula
              exactly (see monitor.rs compute_health_factor).
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #2
=====================================
File(s):     bot/src/main.rs
Line(s):     L223–L254 (evaluate_strategies function)
Category:    COMPLETENESS
Section:     §1.2, §4.1
Description: evaluate_strategies() is a stub. When S2, S3, S4, or S1 are
             enabled, the function logs a trace/debug line and returns.
             No actual strategy logic is called. None of the executor
             functions (try_liquidate, try_execute_best_route, etc.) are
             wired into this loop.

             Additionally, the strategy dispatch inside evaluate_strategies
             uses sequential .await through the async function itself —
             it is called with a single .await from the block handler
             (line 198: evaluate_strategies(...).await) rather than being
             spawned per-strategy with tokio::spawn. This means all four
             strategies block each other.

Operator Law: SKILL.md §4.1 block handler spawn pattern — every strategy
              task must be spawned, never awaited sequentially.
Impact:       Zero revenue. All four strategies are silent. The bot
              subscribes to blocks, computes nothing, submits nothing.
Fix:          (a) Wire the actual executor calls: call try_liquidate for
              each position in S2, try_execute_best_route for S3,
              scan_and_execute for S4, and S1Executor::tick for S1.
              (b) Each strategy evaluation must be spawned:
              tokio::spawn(async move { try_liquidate(...).await });
              rather than awaited inline. They must run in parallel.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #3
=====================================
File(s):     contracts/src/LiquidationExecutor.sol
Line(s):     L178–L191 (_swapCollateralToDebt function)
Category:    COMPLETENESS
Section:     §1.2 TODO/Stub Scan, §12 Solidity Contract Audit
Description: _swapCollateralToDebt() is a skeleton with a comment block
             describing what to implement, plus _approveExact and
             _resetApproval calls around a TODO. The actual DEX call is
             absent. Any call to executeLiquidation() will receive
             collateral, then do nothing with it, then hit
             _assertProfitable() with a zero debt-asset balance and revert.
Operator Law: Part 0 Absolute Ban — no placeholder code in production path.
Impact:       Every S2 liquidation attempt reverts at the profit guard.
              The contract is non-functional for its primary purpose.
Fix:          Implement the swap using the actual DEX router deployed on
              HyperEVM. Use the _swapExact pattern: _approveExact(from,
              dexRouter, amount), call the router's swap function with
              exact input and min output of 0 (profit guard enforces the
              real floor), _resetApproval(from, dexRouter). Verify the
              router interface at deployment.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #4
=====================================
File(s):     contracts/src/TriArbExecutor.sol, contracts/src/StableDepegExecutor.sol,
             contracts/src/EvmArbExecutor.sol
Line(s):     TriArb L126–133, StableDepeg L140–147, EvmArb L238–242
Category:    COMPLETENESS
Section:     §1.2 TODO/Stub Scan
Description: The internal swap helpers _swap(), _stableSwap(), and
             _swapExact() in all three contracts are stubs. They call
             _approveExact and _resetApproval but make no actual DEX call
             in between. Any invocation returns amountOut = 0.

             For TriArbExecutor: _swap() returns 0 → hop loop immediately
             hits "require(amountIn > 0)" and reverts.
             For StableDepegExecutor: _stableSwap() returns 0 → step 1
             check "require(stableBBought > 0)" reverts.
             For EvmArbExecutor: _swapExact() returns 0 → receiveFlashLoan
             requires boughtAmount > 0 and reverts.
Operator Law: Part 0 Absolute Ban — ships working or doesn't ship.
Impact:       S1, S3, S4 contracts are completely non-functional. Every
              invocation reverts. Zero revenue possible.
Fix:          Implement actual DEX call for each pool type present on
              HyperEVM (Uni V2-style: call pool.swap() with computed
              amount0Out/amount1Out; Balancer V3: call vault.swap()).
              The DEX interface must be determined from the actual pools
              deployed on HyperEVM testnet before this can be implemented.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #5
=====================================
File(s):     bot/crates/strategies/evm_arb/src/executor.rs
Line(s):     L123–L131, L293–L295
Category:    SECURITY
Section:     §1.2, SKUA Law 2
Description: Two related bugs in the S1 Phase2Pending state:

             (a) When Phase 2 times out (L123–131), the emergency exit
             cannot be called because held_token is not stored in the
             Phase2Pending variant. The code logs a warning and does
             nothing: "TODO: store held_token in Phase2Pending state".
             An expired Phase 2 leaves tokens permanently stranded in
             the Escrow contract.

             (b) On Phase 1 success (L293–295), held_token is set to
             Address::ZERO instead of the actual purchased token address.
             This zero address propagates into trigger_emergency_exit(),
             which then approves a zero-address token transfer — a no-op
             that leaves real tokens stuck in Escrow.

Operator Law: SKUA Law 2 — Phase 2 must have a functional timeout path.
              Escrow must never permanently hold tokens.
Impact:       HIGH. If Phase 2 does not fill within 30 seconds (e.g.,
              HyperCore congestion, partial fill, price gap closed),
              the tokens held in Escrow cannot be recovered via the bot's
              normal path. Manual contract sweep is the only recovery.
              On active strategies this happens every few hours.
Fix:          (a) Add held_token: Address to the Phase2Pending enum
              variant so the timeout handler has the token address.
              (b) Pass the actual purchased token address to
              Phase1Complete instead of Address::ZERO. The token address
              is the output of the EVM swap — it is boughtToken in the
              receiveFlashLoan callback and must be returned to the bot
              via the Phase1Complete event.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #6
=====================================
File(s):     bot/crates/strategies/evm_arb/src/executor.rs
Line(s):     L380–L386 (trigger_emergency_exit)
Category:    COMPLETENESS
Section:     §1.2 TODO/Stub Scan
Description: trigger_emergency_exit() builds the swap calldata as
             Bytes::new() (empty bytes) and passes dex = Address::ZERO.
             The EvmArbExecutor.emergencyExit() contract function will
             call address(0).call("") which either reverts or succeeds
             as a no-op depending on the EVM. In either case, the held
             token is not swapped. Escrow.emergencyExit() then emits
             an event and clears state — but the token is not recovered.
Operator Law: Part 0 Absolute Ban — emergencyExit must actually exit.
Impact:       HIGH. Emergency exit silently fails to swap the held token.
              Tokens remain in the Escrow after the state machine resets
              to Idle. A subsequent Phase 1 hits "Escrow not empty" and
              the bot is permanently stuck until manual sweep.
Fix:          The bot must build actual swap calldata before calling
              trigger_emergency_exit(). This requires knowing the exact
              DEX router interface at deployment. The swap must route
              held_token → flash_asset (the debt token) using the same
              pool as Phase 1.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #7
=====================================
File(s):     bot/crates/strategies/liquidation/src/sizing.rs
Line(s):     L93
Category:    HARDCODING
Section:     §1.1 Banned Pattern Scan, SKUA Production Law (no hardcoded
             protocol parameters)
Description: The liquidation close factor is hardcoded as 5_000 bps (50%)
             with a comment "TODO: verify with actual HyperLend deployment".

             The SKUA spec explicitly states: "Read close factor from
             protocol — never assume 50%." HyperLend (an Aave V3 fork)
             uses a dynamic close factor: if HF < 0.95, liquidators can
             close 100% of debt. Using a fixed 50% cap means the bot
             will only liquidate half the allowable debt on every position,
             leaving profit on the table and potentially allowing competing
             bots to take the remaining 50%.

Operator Law: SKUA Spec Part 4 S2 — "Read close factor from protocol".
              SKILL.md §7.5 analogue — no fixed percentage caps.
Impact:       MEDIUM-HIGH. S2 systematically under-liquidates every
              position. Bot leaves ~50% of liquidation bonus unrealized
              when HF < 0.95. Competitors take the remainder.
Fix:          The Aave V3 protocol does not expose a simple close_factor
              getter. The correct approach is: read the position's HF,
              if HF < 0.95 use 100% of total_debt, else use 50%. Implement
              this conditional in compute_liquidation_params() based on
              the computed_hf value already available in BorrowerPosition.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #8
=====================================
File(s):     bot/crates/strategies/liquidation/src/sizing.rs
Line(s):     L109–L110, L112–L113
Category:    CALCULATION
Section:     §1.1 Banned Pattern Scan — unwrap_or(0.0) in hot path
Description: debt_price and collateral_price are fetched from the PriceMap
             with .copied().unwrap_or(0.0). If either price is missing,
             the code then immediately checks "if debt_price <= 0.0 ||
             collateral_price <= 0.0 { return Ok(None); }".

             This appears safe but has a subtle bug: unwrap_or(0.0) masks
             the root cause (missing price for an asset in a live position).
             A missing price should be an Err propagated upward so the
             caller knows which asset has no price data, can log it, and
             can investigate whether the price feed is broken. Returning
             Ok(None) silently skips the position on every block, which
             could cause a profitable liquidation to be missed indefinitely
             if the price indexer has a gap.

Operator Law: Part 0 — "unwrap_or(0.0) in hot path" — propagate or fail
              loudly. SKILL.md §1.3 Q2.
Impact:       MEDIUM. A broken price feed for a collateral asset causes
              all positions collateralised by that asset to be silently
              skipped forever. No alert fires. Bot appears healthy.
Fix:          Replace unwrap_or(0.0) with explicit match or:
              let debt_price = prices.get(&debt_asset)
                .copied()
                .ok_or_else(|| anyhow!("No price for debt asset {}", debt_asset))?;
              Propagate the error upward. The caller logs and skips this
              position with a warning, making the gap visible.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #9
=====================================
File(s):     bot/crates/strategies/liquidation/src/sizing.rs
Line(s):     L128–L131
Category:    HARDCODING
Section:     §1.1 Banned Pattern Scan — hardcoded pool values
Description: Swap slippage for the collateral→debt swap is hardcoded as
             0.003 (0.30%) with the comment "must be replaced with AMM
             estimate". This is not live data — it is a fixed constant
             that does not adjust for pool depth, trade size, or market
             conditions.

             For a large liquidation (e.g. $500K collateral), actual
             slippage on a thin pool could be 3–5%. The bot would
             evaluate the trade as profitable with 0.30% slippage, submit,
             and revert at the contract's profit guard — wasting gas and
             incrementing the consecutive-revert counter.
Operator Law: SKUA Spec — "hardcoded pool reserves" and "hardcoded
              liquidation bonus" are in the Absolute Bans list.
Impact:       MEDIUM. Every large liquidation on illiquid collateral will
              be overestimated as profitable, submitted, and revert. The
              consecutive-revert circuit breaker (threshold 3) will fire
              after three such attempts, halting the entire bot.
Fix:          Replace the hardcoded slippage with an AMM computation.
              Use the AmmPool::get_output() function with the live
              pool reserves to compute the actual output for the expected
              swap size. The pool address for the collateral→debt route
              must be resolved from the token pair at execution time.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #10
=====================================
File(s):     bot/crates/simulation/src/amm.rs
Line(s):     L111–L113, L141–L143
Category:    CALCULATION
Section:     §6.2 F-13: Relative Convergence in D and Y Loops
Description: Both compute_d() and compute_y() use ABSOLUTE convergence:
               if (d - d_prev).abs() <= 1.0 { break; }
               if (y - y_prev).abs() <= 1.0 { break; }

             The audit protocol requires RELATIVE convergence:
               if d > 0.0 && (d - d_prev).abs() / d <= 1e-8 { break; }

             For pools with large reserve values (e.g. a $500M USDC-USDT
             pool where reserves are represented as ~500_000_000_000_000
             in 6-decimal units), the absolute tolerance of 1.0 is
             enormous. The loop will terminate after very few iterations
             with a D value that is off by millions of dollars in pool
             units. This produces incorrect get_output_stable() results
             for any large pool.

Operator Law: SKILL.md §6.2 — F-13 production failure mode. Absolute
              convergence breaks for pools with balances > $100M.
Impact:       HIGH on large pools. Profit estimates diverge from reality
              by potentially thousands of dollars per trade. The eth_call
              simulation gate (layer 5) will catch the divergence (>0.5%)
              and abort — preventing losses — but the strategy will never
              execute on large pools, leaving significant profit uncaptured.
Fix:          Replace both convergence checks with relative form:
              // In compute_d:
              if d > 0.0 && (d - d_prev).abs() / d <= 1e-8 { break; }
              // In compute_y:
              if y > 0.0 && (y - y_prev).abs() / y <= 1e-8 { break; }
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #11
=====================================
File(s):     bot/crates/simulation/src/amm.rs
Line(s):     L71–L73 (get_output_stable)
Category:    CALCULATION
Section:     §6.1 F-10: Pool Fee — Applied on Wrong Side for Curve Pools
Description: get_output_stable() applies the fee on the INPUT side:
               let fee_factor = 1.0 - self.fee_bps / 10_000.0;
               let new_reserve_in = self.reserve_in + amount_in * fee_factor;

             The Curve StableSwap specification applies the fee to the
             OUTPUT (dy): dy_with_fee = dy_raw - dy_raw * fee_rate.

             Applying the fee to the input is the Uniswap V2 convention.
             For the StableSwap invariant, fee-on-input changes which
             invariant is solved (it uses a deflated x rather than
             computing the full output then deducting fee). The numerical
             difference is small for low fees (~0.04%) but creates a
             systematic bias: fee-on-input slightly overstates output
             compared to the correct fee-on-output form. This is the
             same class of error as F-10 in SKILL.md.

Operator Law: SKILL.md §6.1 — "fee is applied to OUTPUT, not to input dx".
Impact:       LOW-MEDIUM. Systematic overestimation of stable pool output
              by a small fraction (~fee²). For a $1M trade at 0.04% fee
              this is ~$0.16 overestimation. The profit guard prevents
              actual losses but causes occasional false positives at the
              margin that revert on-chain.
Fix:          Implement fee-on-output for get_output_stable():
              1. Compute new_reserve_in = self.reserve_in + amount_in
                 (no fee reduction on input)
              2. Solve for new_reserve_out = compute_y(...)
              3. dy_raw = self.reserve_out - new_reserve_out - 1.0
              4. Apply fee: fee = dy_raw * self.fee_bps / 10_000.0
              5. Return: if dy_raw <= fee { 0.0 } else { dy_raw - fee }
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #12
=====================================
File(s):     bot/crates/simulation/src/sizing.rs
Line(s):     L43–L57 (optimal_borrow_size)
Category:    ARCHITECTURE
Section:     §7.1 Stage 1 — Imbalance-Anchored Upper Bound
Description: The GSS sizing engine skips Stage 1 (imbalance-anchored
             probe) entirely. The upper bound is set directly to
             min(flash_available_usd, hard_cap_usd) without an
             imbalance-anchored starting probe or a doubling loop.

             The audit protocol requires Stage 1:
               probe_frac = (imbalance * 0.5).max(0.005).min(0.20)
               probe = bal_a_i * probe_frac
               doubling loop: while profit_positive(probe) && probe < bal_a_i * 0.95
               upper_bound = probe (output of doubling loop)

             Going straight to the full flash_available ceiling wastes
             128 GSS iterations across a domain where 90%+ of the range
             is either zero profit (too large, too much slippage) or
             requires extreme liquidity that may not be available in
             practice. The 128-iteration GSS still finds the correct
             maximum, but requires the profit function to be well-behaved
             across the full range — which it may not be if flash
             liquidity is quoted higher than actual pool depth.

Operator Law: SKILL.md §7.1 — imbalance-anchored probe is Stage 1 of
              the mandatory four-stage sizing pipeline.
Impact:       LOW-MEDIUM. GSS still converges correctly in most cases.
              Risk is in edge cases where profit function has local
              flat regions near zero at extreme sizes, potentially
              causing premature convergence away from the true maximum.
Fix:          Add Stage 1 before GSS. Compute imbalance_ratio from
              pool reserves, derive probe_frac, run the doubling loop
              to establish the upper bound, then pass that bound as hi
              to the GSS.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #13
=====================================
File(s):     bot/crates/simulation/src/sizing.rs
Line(s):     (absent — Stage 3 not implemented)
Category:    ARCHITECTURE
Section:     §7.3 Stage 3 — Dual-Leg A-Parameter Impact Gate
Description: Stage 3 (dual-leg A-parameter impact gate) is completely
             absent. There is no max_impact_for_a() function, no
             gated_exit_mid check, and no proportional scaling of entry
             size when the exit pool is the binding constraint.

             The sizing pipeline goes directly from GSS to the profit
             floor check. This means for strategies routing through
             stable pools with different amplification coefficients
             (e.g. a high-A entry pool into a low-A exit pool), the
             optimal borrow size can systematically oversize into the
             shallow exit pool.

Operator Law: SKILL.md §7.3 — "the gate is applied to BOTH pool_a
              (entry leg) AND pool_b (exit leg). NOT a single-leg gate."
Impact:       MEDIUM. On imbalanced A-coefficient routes, the bot may
              size into the exit pool beyond its effective depth. The
              AMM math will still compute a profit (because slippage
              is modeled), but the simulation layer will diverge vs
              the algebraic estimate on these routes, causing aborts.
              On routes where the two legs have very different A values,
              the sizing is materially wrong.
Fix:          Implement Stage 3 after Stage 1 (doubling loop) and before
              Stage 2 (GSS). Compute max_impact_for_a() for both pools,
              run GSS bounded by the tighter of the two impact gates.
              If exit pool is the binding constraint, scale entry size
              proportionally.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #14
=====================================
File(s):     bot/crates/strategies/triangular/src/graph.rs
Line(s):     L383–L385
Category:    SECURITY
Section:     §1.1 Banned Pattern Scan — Address::ZERO in non-test code
Description: The unit test for rates_product uses Address::ZERO for both
             pool and token_out fields of a PoolEdge struct. This is
             inside a #[cfg(test)] block and is acceptable.

             However, in the graph construction code at L285, the
             topic hash parsing uses .unwrap() on a B256 parse inside
             the liquidation monitor (bot/crates/strategies/liquidation/
             src/monitor.rs:144):
               .map(|t| t.parse().unwrap())
             This panics at startup if any of the hardcoded hex strings
             are malformed. On a production bot a panic kills the process
             and requires manual restart.

Operator Law: Part 0 — no .unwrap() in hot path.
Impact:       LOW-MEDIUM. Malformed topic hash constant causes panic at
              startup. Bot fails to start. Requires code fix and
              redeployment.
Fix:          Replace .unwrap() with error propagation:
              .map(|t| t.parse::<B256>()
                .context("Invalid event topic hash"))
              .collect::<Result<Vec<_>>>()?
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #15
=====================================
File(s):     bot/crates/strategies/evm_arb/src/executor.rs
Line(s):     L105 (S1Phase::Phase1Complete match arm)
Category:    SECURITY
Section:     §1.1 Banned Pattern Scan
Description: When evaluating whether to trigger emergency exit, the
             expected_profit passed to should_emergency_exit() is
             hardcoded as 0.0 with a comment "TODO: store in phase".

             The should_emergency_exit() function uses expected_profit
             to compute the EMERGENCY_EXIT_LOSS_THRESHOLD check:
               loss_usd > expected_profit * EMERGENCY_EXIT_LOSS_THRESHOLD

             With expected_profit = 0.0, the threshold is 0.0 × 0.5 = 0.0.
             Any positive loss_usd > 0.0 immediately triggers emergency
             exit. This makes the price-move guard fire on the first
             tick after Phase 1 completes, before Phase 2 has any chance
             to execute — even when the price move is within normal
             variance. The bot will emergency-exit nearly every Phase 1
             position immediately.
Operator Law: SKUA Law 2 — two-phase state machine must function correctly.
Impact:       HIGH. S1 is inoperable. Every Phase 1 immediately triggers
              emergency exit on the next tick, realising a guaranteed loss
              (emergency exit sells at market, not at the planned CoreWriter
              bid). The strategy systematically loses money instead of
              making it.
Fix:          Store expected_profit_usd in the Phase1Complete enum variant.
              Pass the sizing result's expected_profit to the state when
              transitioning to Phase1Complete. Use that stored value in
              should_emergency_exit().
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #16
=====================================
File(s):     bot/crates/api/src/metrics.rs, all strategy crates
Line(s):     All metric counter/histogram declarations
Category:    COMPLETENESS
Section:     §15.1 Prometheus Metrics — Complete Set
Description: Metrics are registered but NEVER INCREMENTED. No call to
             OPPS_DETECTED.with_label_values(&["s2"]).inc() or any
             equivalent exists anywhere outside the metrics registration
             file. TXS_LANDED, PROFIT_USD, LATENCY_SIGNAL_TO_SUBMIT_MS,
             SIM_DIVERGENCE_PCT — all registered, all always zero.

             Additionally, five of the twelve required metrics from the
             audit protocol are absent:
             - {strategy}_bundles_fired_total (absent — distinct from submitted)
             - {strategy}_bundles_landed_total (absent — TXS_LANDED exists
               but is never incremented from receipts)
             - {strategy}_landing_rate (absent — most important ratio)
             - {strategy}_block_latency_ms (absent)
             - {strategy}_reverts_by_class (absent — single undifferentiated
               TXS_REVERTED exists but no per-reason breakdown)

Operator Law: SKILL.md §15.1 — all 12 metrics required.
              profit_usd_actual must come from on-chain receipts.
Impact:       MEDIUM. Prometheus dashboard shows all zeros. Landing rate
              (the most important ratio) is uncomputable. Gas kill threshold
              alert cannot fire reliably because GAS_HYPE_TOTAL is never
              incremented. Operator is flying blind.
Fix:          (a) Wire increment calls into every strategy execution path:
              after submit_sync() succeeds, increment TXS_LANDED and
              record profit from the receipt. After every simulation pass,
              increment SIMS_PASSED. After each signal detection, increment
              OPPS_DETECTED.
              (b) Add the five missing metrics.
              (c) Record LATENCY_SIGNAL_TO_SUBMIT_MS by capturing
              Instant::now() on signal detection and recording elapsed
              time after submit_sync() returns.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #17
=====================================
File(s):     bot/src/main.rs, bot/crates/api/src/telegram.rs
Line(s):     main.rs L191–L192 (only alert wired)
Category:    COMPLETENESS
Section:     §15.2 Alert Thresholds
Description: Of the six required Telegram alerts, only one is wired:
             alert_zero_price() fires when the HYPE price precompile
             returns 0.

             These five are defined in TelegramAlerter but never called:
             - alert_kill_switch() — kill switch triggers (gas kill,
               consecutive reverts) never notify Telegram
             - alert_low_balance() — wallet balance monitoring is absent
             - alert_sim_divergence() — divergence check in eth_call.rs
               logs a warning but does not call Telegram
             - landing rate < 20% — not computed, not alerted
             - nonce gap — not monitored, not alerted

Operator Law: SKILL.md §15.2 — all 6 alerts must be wired to Telegram.
Impact:       MEDIUM. Operator has no real-time notification when the
              circuit breaker fires, wallets run low, or the strategy
              starts systematically reverting. First notice may be when
              the profit wallet has nothing in it.
Fix:          Wire Telegram calls into:
              (a) BotState::halt() — call alert_kill_switch(reason)
              (b) submit_sync() success path — check wallet balance after
              and call alert_low_balance() if < 0.05 HYPE
              (c) eth_call.rs divergence branch — call alert_sim_divergence()
              (d) Add a landing-rate tracker that computes TXS_LANDED /
              TXS_SUBMITTED per hour and alerts if < 20%
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #18
=====================================
File(s):     contracts/src/TriArbExecutor.sol, contracts/src/StableDepegExecutor.sol
Line(s):     TriArb L84, StableDepeg L93
Category:    SECURITY
Section:     §12.2 Balancer Callback Security
Description: The receiveFlashLoan() callbacks in TriArbExecutor and
             StableDepegExecutor verify msg.sender == balancerVault but
             do NOT verify that the callback was initiated by this
             contract itself. The Balancer vault does not pass an
             initiator parameter — but an attacker can call Balancer's
             flashLoan() with a crafted userData, setting the recipient
             as this contract. The msg.sender check passes (it is the
             genuine Balancer vault), but the callback is executing
             adversarial userData.

             Correct mitigation: encode address(this) into userData when
             initiating, then decode and verify it in the callback:
               require(initiator == address(this), "Not self-initiated");
             This pattern is implemented correctly in LiquidationExecutor
             (which uses the Aave initiator parameter) but is absent from
             the two Balancer-based contracts.

Operator Law: SKILL.md §12.2 — "initiator verification via userData
              encoding".
Impact:       HIGH. An attacker can craft a Balancer flash loan call that
              triggers receiveFlashLoan() on TriArbExecutor or
              StableDepegExecutor with adversarial swap calldata, potentially
              draining any tokens left in the contract or manipulating
              the swap path to extract profit.
Fix:          In the entry point functions (executeTriArb,
              executeDepegArb): encode address(this) into userData:
                bytes memory userData = abi.encode(address(this), hops, minProfitWei);
              In receiveFlashLoan(): decode and verify:
                (address initiator, ...) = abi.decode(userData, (address, ...));
                require(initiator == address(this), "Not self-initiated");
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #19
=====================================
File(s):     contracts/src/EvmArbExecutor.sol
Line(s):     L271–L274 (_amountInForExactOut)
Category:    HARDCODING
Section:     §1.1 Banned Pattern Scan
Description: _amountInForExactOut() hardcodes the pool fee as 997/1000
             (0.30% Uni V2 standard fee) with the comment "TODO: read
             actual fee from pool at deployment". This is used to compute
             how much of the purchased token to sell back in order to
             exactly repay the flash loan in Phase 1.

             If the actual pool fee differs from 0.30% (e.g. it is 0.05%
             for a stablecoin pair, or 1.0% for an exotic pair), the
             computed toSellBack amount will be wrong. Too little → the
             flash loan repayment is under-funded and the callback reverts.
             Too much → excess tokens are sold back unnecessarily, reducing
             the escrow amount below minEscrowAmount, causing a revert.

Operator Law: SKUA Production Law — no hardcoded pool fee. Read from pool.
Impact:       MEDIUM. Phase 1 fails on any pool whose fee is not exactly
              0.30%. The strategy only works on standard Uni V2 0.30%
              pools until this is fixed.
Fix:          Read the fee from the pool at execution time. For Uni V2-style
              pools, call pair.fee() or check the factory. Use the live
              fee value in the getAmountIn formula:
              uint256 denominator = (reserveOut - amountOut) * (10_000 - fee_bps);
              where fee_bps is read from the pool.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #20
=====================================
File(s):     contracts/src/EvmArbExecutor.sol
Line(s):     L217–L227 (emergencyExit function)
Category:    COMPLETENESS
Section:     §1.2, SKUA Law S1 Escrow
Description: The netResult computation in emergencyExit() is a tautology:
               int256 netResult = int256(amount) - int256(amount); // = 0

             The EmergencyExit event always emits netResult = 0 regardless
             of the actual outcome. The bot cannot distinguish a breakeven
             emergency exit from a loss or a gain. Additionally, the
             function does not sweep remaining tokens of the debt asset
             back to profitWallet — the comment says "sweep() can be
             called separately" but requires a manual transaction.

Operator Law: SKUA Spec — "Sweep to owner regardless of profit/loss".
Impact:       LOW-MEDIUM. P&L accounting is wrong for every emergency
              exit. The bot logs the event as a zero net result. Operator
              cannot accurately track S1 P&L. Tokens require a manual
              sweep call to recover — adding latency and operational burden.
Fix:          Before the external DEX call, record the balance of the debt
              asset. After the call, record the new balance. Compute:
              netResult = int256(debtBalAfter) - int256(heldAmount).
              After the swap, call IERC20(debtToken).safeTransfer(
              profitWallet, debtBalAfter) in the same transaction.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #21
=====================================
File(s):     bot/Cargo.toml
Line(s):     [profile.release] block
Category:    CONFIG
Section:     §2.3 Release Profile
Description: The release profile is missing panic = "abort".

             Current profile:
               [profile.release]
               opt-level     = 3
               lto           = "thin"
               codegen-units = 1

             Required:
               panic = "abort"

             Without panic = "abort", the Rust runtime unwinds the stack
             on panic, which: (a) adds ~5KB to the binary size, (b) can
             take several milliseconds during unwinding while holding locks,
             and (c) creates potential for partially-unwound state in async
             tasks that share resources with the block loop.

Operator Law: SKILL.md §2.3 — panic = "abort" is required in release profile.
Impact:       LOW. No functional impact in normal operation. On a panic
              (which should never happen in production code), unwinding
              is slower and potentially leaves shared state inconsistent
              versus the clean process termination of abort.
Fix:          Add panic = "abort" to the [profile.release] block in
              bot/Cargo.toml.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #22
=====================================
File(s):     bot/Cargo.toml
Line(s):     alloy version declaration
Category:    CONFIG
Section:     §2.2 Dependency Versions
Description: The alloy crate is pinned to version "0.3". The audit
             protocol requires alloy ≥ 2.0. alloy 0.3 is a pre-1.0
             release with different API surfaces, particularly around
             provider construction, transaction building, and the
             EthereumWallet type. Several API calls in the codebase
             (e.g. on_ws(), on_http(), EthereumWallet::from()) use
             patterns from alloy 0.x that may not compile against
             alloy ≥ 2.0 without changes, but the target version
             is not being met.

             Additionally, revm is absent from all Cargo.toml files.
             Layer 5 simulation uses eth_call (which is correct for
             HyperEVM's non-standard chain), but the audit protocol
             expects revm for local fork simulation. On HyperEVM,
             eth_call to a private node is the acceptable substitute,
             but this should be explicitly documented.

Operator Law: SKILL.md §2.2 — alloy ≥ 2.0, no ethers-rs.
Impact:       LOW. alloy 0.3 is usable but lacks stability guarantees.
              API breaking changes in 1.0+ will require migration.
Fix:          Evaluate migration to alloy ≥ 2.0. Add a comment in
              Cargo.toml explaining why revm is not used (HyperEVM
              eth_call substitution) to prevent confusion.
Status:       [ ] OPEN
=====================================
```

---

```
=====================================
FINDING #23
=====================================
File(s):     contracts/script/Deploy.s.sol
Line(s):     entire script
Category:    ARCHITECTURE
Section:     §20.1 Deployment Script Audit, §19.3 Chain ID consistency
Description: The deployment script has no chain ID guard. It does not
             check block.chainid before deploying. If the script is
             accidentally run against the wrong network (e.g. a testnet
             fork that responds on the same port), contracts deploy to
             the wrong chain with no error.

             The deploy script also has no post-deployment verification
             step — it does not call cast call to confirm the deployed
             contract's immutables (hyperLendPool, balancerVault,
             profitWallet) match the expected values.

             Additionally, the bot's submit.rs hardcodes chain_id(999)
             inline (L39) rather than reading it from SkuaConfig.
             If the chain ID changes (e.g. HyperEVM testnet uses a
             different ID), submit.rs must be manually updated rather
             than inheriting from config.

Operator Law: SKILL.md §20.1 — deployment script must check block.chainid.
              §19.3 — chain ID must be consistent across bot and contract.
Impact:       LOW-MEDIUM. Accidental wrong-network deployment is possible.
              Hardcoded chain ID in submit.rs is a maintenance hazard.
Fix:          (a) Add to Deploy.s.sol: require(block.chainid == 999,
              "Wrong chain");
              (b) Add post-deployment verification in the script using
              vm.assertEq(deployed.hyperLendPool(), vm.envAddress(...)).
              (c) Read chain_id from config in submit.rs:
              .chain_id(config.chain_id) rather than hardcoded 999.
Status:       [ ] OPEN
=====================================
```

---

## SUMMARY TABLE

| # | File(s) | Category | Impact | Status |
|---|---|---|---|---|
| 1 | liquidation/src/monitor.rs | COMPLETENESS | S2 inert — zero liquidations | OPEN |
| 2 | src/main.rs | COMPLETENESS | All strategies inert — zero revenue | OPEN |
| 3 | LiquidationExecutor.sol | COMPLETENESS | S2 reverts on every call | OPEN |
| 4 | TriArb/StableDepeg/EvmArb .sol | COMPLETENESS | S1/S3/S4 revert on every call | OPEN |
| 5 | evm_arb/src/executor.rs | SECURITY | Phase 2 timeout cannot recover | OPEN |
| 6 | evm_arb/src/executor.rs | SECURITY | Emergency exit is a no-op | OPEN |
| 7 | liquidation/src/sizing.rs | HARDCODING | Under-liquidates every position | OPEN |
| 8 | liquidation/src/sizing.rs | CALCULATION | Silent price-missing mask | OPEN |
| 9 | liquidation/src/sizing.rs | HARDCODING | Fixed slippage causes reverts | OPEN |
| 10 | simulation/src/amm.rs | CALCULATION | F-13: absolute convergence breaks large pools | OPEN |
| 11 | simulation/src/amm.rs | CALCULATION | F-10 variant: fee on wrong side for stable pools | OPEN |
| 12 | simulation/src/sizing.rs | ARCHITECTURE | Stage 1 (imbalance probe) absent | OPEN |
| 13 | (absent) | ARCHITECTURE | Stage 3 (dual-leg impact gate) absent | OPEN |
| 14 | liquidation/src/monitor.rs | SECURITY | .unwrap() on topic hash panics at startup | OPEN |
| 15 | evm_arb/src/executor.rs | SECURITY | expected_profit=0 forces immediate emergency exit | OPEN |
| 16 | api/src/metrics.rs | COMPLETENESS | Metrics registered but never incremented | OPEN |
| 17 | src/main.rs, api/telegram.rs | COMPLETENESS | 5 of 6 Telegram alerts not wired | OPEN |
| 18 | TriArbExecutor.sol, StableDepegExecutor.sol | SECURITY | Balancer callback missing initiator verify | OPEN |
| 19 | EvmArbExecutor.sol | HARDCODING | Pool fee hardcoded 0.30% in repay calc | OPEN |
| 20 | EvmArbExecutor.sol | COMPLETENESS | Emergency exit netResult = 0 always | OPEN |
| 21 | bot/Cargo.toml | CONFIG | panic=abort missing from release profile | OPEN |
| 22 | bot/Cargo.toml | CONFIG | alloy 0.3 < required ≥ 2.0 | OPEN |
| 23 | Deploy.s.sol, submit.rs | ARCHITECTURE | No chain ID guard; hardcoded 999 in submit | OPEN |

---

## FINDINGS THAT ARE CLEAN (PASSING)

The following items from the audit protocol were checked and found satisfactory:

- **SkuaBase constructor zero-address guards** — all three checked and non-zero enforced
- **_assertProfitable on-chain guard** — present in all four contracts, never bypassable
- **Transient storage reentrancy guard (EIP-1153)** — correctly implemented with tload/tstore
- **type(uint256).max approval** — absent; exact approvals with reset used throughout
- **_safeOraclePx / _safeSpotPx zero-price revert** — correctly implemented, reverts on zero
- **L1READ and COREWRITER as constants** — correct immutable constants, never storage slots
- **Flash fee read at runtime** — Balancer reads feeAmounts[0] from callback; HyperLend reads FLASHLOAN_PREMIUM_TOTAL() at execution time
- **CoreWriter never in same tx as repay** — S1 Phase 1 and Phase 2 are correctly separated
- **Net profit formula** — all three profit functions include flash fee + gas cost, return NEG_INFINITY on bad inputs
- **GSS 128 iterations** — correct constant, correct phi, correct return (lo+hi)/2
- **Dynamic profit floor** — correct formula: strategy_min.max(gas_roi * gas_usd)
- **Wallet pool LRU acquire** — compare_exchange race-free, fetch_add nonce atomic
- **Kill switch checked before every submission** — correct in submit_sync
- **eth_sendRawTransactionSync** — used exclusively, no fire-and-forget
- **WS subscription, never HTTP for blocks** — block_subscription_loop uses subscribe_blocks on WsProvider
- **Fee refresh loop** — runs every 1000 blocks, reads both HyperLend and Balancer fees on-chain
- **Balancer fee WAD conversion** — correctly divides by 1e18 to get bps
- **executeOperation double guard** — msg.sender AND initiator both checked in LiquidationExecutor
- **.env in .gitignore** — confirmed present on line 2
- **Config from_env hard fails on missing vars** — no fallback defaults for capital params
- **S4 oracle never hardcoded as 1.0** — check_depeg uses live oracle price, returns None on zero
- **Zero price guard in profit functions** — all three return NEG_INFINITY on zero mid/out
- **Phase2 timeout constant = 30s** — correct in both contract and bot
- **Consecutive revert circuit breaker** — wired in submit_sync, halt() called on threshold

---

## CRITICAL PATH TO PRODUCTION

These findings must be resolved in this order before any capital is deployed:

**Block 1 (must fix before any testing):**
- Finding #2: Wire strategy executors into main loop
- Finding #1: Implement refresh_borrower_position
- Finding #3/#4: Implement DEX swaps in all contracts
- Finding #18: Add Balancer initiator verification

**Block 2 (must fix before testnet run):**
- Finding #5/#6: Fix S1 Phase2Pending held_token + emergency exit calldata
- Finding #15: Fix expected_profit=0 in Phase1Complete state
- Finding #14: Fix .unwrap() panic on topic hash
- Finding #10: Fix absolute convergence (F-13)

**Block 3 (must fix before mainnet):**
- Finding #7: Live close factor from protocol
- Finding #8: Propagate price-missing as error
- Finding #9: Live slippage from AMM math
- Finding #11: Fix stable pool fee side (F-10 variant)
- Finding #16/#17: Wire metrics and Telegram alerts
- Finding #19: Read pool fee in _amountInForExactOut
- Finding #20: Fix emergency exit P&L accounting

**Block 4 (housekeeping, fix before mainnet):**
- Finding #12/#13: Add sizing stages 1 and 3
- Finding #21: Add panic=abort
- Finding #22: Evaluate alloy version
- Finding #23: Add chain ID guard to deploy script
