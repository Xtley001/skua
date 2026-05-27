// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {LiquidationExecutor} from "../src/LiquidationExecutor.sol";
import {TriArbExecutor}      from "../src/TriArbExecutor.sol";
import {StableDepegExecutor} from "../src/StableDepegExecutor.sol";
import {EvmArbExecutor}      from "../src/EvmArbExecutor.sol";

/// @notice SKUA deployment script.
/// AUDIT FIX #23: chain ID guard added — reverts if deployed to wrong chain.
/// Deploy order: S2 → S3 → S4 → S1 (per build spec Part 11).
contract DeploySkua is Script {

    /// @dev Target chain ID. Change for testnet deployments.
    uint256 constant TARGET_CHAIN_ID = 999; // HyperEVM mainnet

    function run() external {
        // AUDIT FIX #23: explicit chain ID guard — prevents accidental wrong-network deploy
        require(
            block.chainid == TARGET_CHAIN_ID,
            string(abi.encodePacked(
                "Wrong chain: expected ", vm.toString(TARGET_CHAIN_ID),
                " but got ", vm.toString(block.chainid)
            ))
        );

        address profitWallet  = vm.envAddress("SKUA_PROFIT_WALLET");
        address balancerVault = vm.envAddress("SKUA_BALANCER_VAULT");
        address hyperLendPool = vm.envAddress("SKUA_HYPERLEND_POOL");
        address dexRouter     = vm.envAddress("SKUA_DEX_ROUTER");

        require(profitWallet  != address(0), "SKUA_PROFIT_WALLET not set");
        require(balancerVault != address(0), "SKUA_BALANCER_VAULT not set");
        require(hyperLendPool != address(0), "SKUA_HYPERLEND_POOL not set");
        require(dexRouter     != address(0), "SKUA_DEX_ROUTER not set");

        vm.startBroadcast();

        // ── S2: Liquidation ───────────────────────────────────────────────
        LiquidationExecutor liquidation = new LiquidationExecutor(
            profitWallet, balancerVault, hyperLendPool, dexRouter
        );
        console2.log("LiquidationExecutor:", address(liquidation));

        // ── Post-deploy verification ──────────────────────────────────────
        require(liquidation.owner()         == msg.sender,    "liq: wrong owner");
        require(liquidation.profitWallet()  == profitWallet,  "liq: wrong profitWallet");
        require(liquidation.balancerVault() == balancerVault, "liq: wrong balancerVault");
        require(liquidation.hyperLendPool() == hyperLendPool, "liq: wrong hyperLendPool");
        require(liquidation.dexRouter()     == dexRouter,     "liq: wrong dexRouter");

        // ── S3: Triangular arb ────────────────────────────────────────────
        TriArbExecutor triArb = new TriArbExecutor(
            profitWallet, balancerVault, hyperLendPool
        );
        console2.log("TriArbExecutor:", address(triArb));
        require(triArb.profitWallet()  == profitWallet,  "tri: wrong profitWallet");

        // ── S4: Stable depeg ──────────────────────────────────────────────
        StableDepegExecutor stableDepeg = new StableDepegExecutor(
            profitWallet, balancerVault, hyperLendPool
        );
        console2.log("StableDepegExecutor:", address(stableDepeg));
        require(stableDepeg.profitWallet() == profitWallet, "depeg: wrong profitWallet");

        // ── S1: EVM/Core arb (last — most complex) ────────────────────────
        EvmArbExecutor evmArb = new EvmArbExecutor(
            profitWallet, balancerVault, hyperLendPool
        );
        console2.log("EvmArbExecutor:", address(evmArb));
        require(evmArb.profitWallet()  == profitWallet,  "evmArb: wrong profitWallet");
        // Verify escrow starts in state 0
        require(evmArb.phase()      == 0,          "evmArb: non-zero initial phase");
        require(evmArb.heldToken()  == address(0), "evmArb: non-zero initial heldToken");
        require(evmArb.heldAmount() == 0,          "evmArb: non-zero initial heldAmount");

        vm.stopBroadcast();

        console2.log("\n=== Paste into .env ===");
        console2.log("SKUA_CONTRACT_LIQ=%s",     address(liquidation));
        console2.log("SKUA_CONTRACT_TRI=%s",     address(triArb));
        console2.log("SKUA_CONTRACT_DEPEG=%s",   address(stableDepeg));
        console2.log("SKUA_CONTRACT_EVM_ARB=%s", address(evmArb));
    }
}
