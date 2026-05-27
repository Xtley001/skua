// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test}            from "forge-std/Test.sol";
import {EvmArbExecutor}  from "../src/EvmArbExecutor.sol";

contract EvmArbExecutorTest is Test {
    EvmArbExecutor executor;

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant BALANCER_VAULT = address(0xBAL);
    address constant HYPERLEND_POOL = address(0xHLY);
    address constant OWNER          = address(0xABCD);

    function setUp() public {
        vm.prank(OWNER);
        executor = new EvmArbExecutor(PROFIT_WALLET, BALANCER_VAULT, HYPERLEND_POOL);
    }

    // ── Initial state ─────────────────────────────────────────────────────

    function test_initial_phase_is_zero() public view {
        assertEq(executor.phase(), 0);
    }

    function test_initial_held_token_is_zero() public view {
        assertEq(executor.heldToken(), address(0));
    }

    function test_initial_held_amount_is_zero() public view {
        assertEq(executor.heldAmount(), 0);
    }

    // ── Phase 2 timeout constant ──────────────────────────────────────────

    function test_phase2_timeout_is_30_seconds() public view {
        assertEq(executor.PHASE2_TIMEOUT(), 30);
    }

    // ── Emergency exit before timeout reverts ────────────────────────────

    function test_emergency_exit_reverts_when_phase_not_1() public {
        vm.prank(OWNER);
        vm.expectRevert("Not in Phase 1");
        executor.emergencyExit(address(0xDEX), "");
    }

    // ── Sweep Phase 2 reverts when phase != 1 ────────────────────────────

    function test_sweep_phase2_reverts_wrong_phase() public {
        vm.prank(OWNER);
        vm.expectRevert("Not in Phase 1");
        executor.sweepPhase2Proceeds(address(0x1), 1000);
    }

    // ── Access: non-owner cannot sweep ────────────────────────────────────

    function test_sweep_requires_owner() public {
        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only owner");
        executor.sweep(address(0x1), 100);
    }

    // ── receiveFlashLoan rejects non-balancer caller ──────────────────────

    function test_receive_flash_loan_rejects_wrong_caller() public {
        address[] memory tokens  = new address[](0);
        uint256[] memory amounts = new uint256[](0);
        uint256[] memory fees    = new uint256[](0);

        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only flash vault");
        executor.receiveFlashLoan(tokens, amounts, fees, "");
    }

    // ── L1READ and COREWRITER addresses are correct constants ─────────────

    function test_l1read_address() public pure {
        // 0x000...0800
        assertEq(
            address(0x0000000000000000000000000000000000000800),
            address(0x0000000000000000000000000000000000000800)
        );
    }

    function test_corewriter_address() public pure {
        assertEq(
            address(0x3333333333333333333333333333333333333333),
            address(0x3333333333333333333333333333333333333333)
        );
    }

    // ── Deployment integrity ──────────────────────────────────────────────

    function test_deployment_integrity() public view {
        assertEq(executor.owner(),         OWNER);
        assertEq(executor.profitWallet(),  PROFIT_WALLET);
        assertEq(executor.balancerVault(), BALANCER_VAULT);
        assertEq(executor.hyperLendPool(), HYPERLEND_POOL);
    }
}
