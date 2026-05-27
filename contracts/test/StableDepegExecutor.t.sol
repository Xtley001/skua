// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test}                 from "forge-std/Test.sol";
import {StableDepegExecutor}  from "../src/StableDepegExecutor.sol";

contract StableDepegExecutorTest is Test {
    StableDepegExecutor executor;

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant BALANCER_VAULT = address(0xBAL);
    address constant HYPERLEND_POOL = address(0xHLY);
    address constant OWNER          = address(0xABCD);

    function setUp() public {
        vm.prank(OWNER);
        executor = new StableDepegExecutor(PROFIT_WALLET, BALANCER_VAULT, HYPERLEND_POOL);
    }

    function test_deployment_integrity() public view {
        assertEq(executor.owner(),         OWNER);
        assertEq(executor.profitWallet(),  PROFIT_WALLET);
        assertEq(executor.balancerVault(), BALANCER_VAULT);
        assertEq(executor.hyperLendPool(), HYPERLEND_POOL);
    }

    function test_execute_requires_owner() public {
        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only owner");
        executor.executeDepegArb(address(0x1), 1000, address(0x2), address(0x3), 0, 1);
    }

    function test_execute_rejects_zero_flash_asset() public {
        vm.prank(OWNER);
        vm.expectRevert("flashAsset zero");
        executor.executeDepegArb(address(0), 1000, address(0x2), address(0x3), 0, 1);
    }

    function test_execute_rejects_zero_amount() public {
        vm.prank(OWNER);
        vm.expectRevert("flashAmount zero");
        executor.executeDepegArb(address(0x1), 0, address(0x2), address(0x3), 0, 1);
    }

    function test_execute_rejects_zero_pool_buy() public {
        vm.prank(OWNER);
        vm.expectRevert("poolBuy zero");
        executor.executeDepegArb(address(0x1), 1000, address(0), address(0x3), 0, 1);
    }

    function test_execute_rejects_zero_pool_sell() public {
        vm.prank(OWNER);
        vm.expectRevert("poolSell zero");
        executor.executeDepegArb(address(0x1), 1000, address(0x2), address(0), 0, 1);
    }

    function test_receive_flash_loan_rejects_non_vault() public {
        address[] memory t = new address[](0);
        uint256[] memory a = new uint256[](0);
        uint256[] memory f = new uint256[](0);
        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only flash vault");
        executor.receiveFlashLoan(t, a, f, "");
    }

    function test_sweep_requires_owner() public {
        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only owner");
        executor.sweep(address(0x1), 100);
    }
}
