// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test}            from "forge-std/Test.sol";
import {TriArbExecutor}  from "../src/TriArbExecutor.sol";

contract TriArbExecutorTest is Test {
    TriArbExecutor executor;

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant BALANCER_VAULT = address(0xBAL);
    address constant HYPERLEND_POOL = address(0xHLY);
    address constant OWNER          = address(0xABCD);

    function setUp() public {
        vm.prank(OWNER);
        executor = new TriArbExecutor(PROFIT_WALLET, BALANCER_VAULT, HYPERLEND_POOL);
    }

    function test_deployment_integrity() public view {
        assertEq(executor.owner(),         OWNER);
        assertEq(executor.profitWallet(),  PROFIT_WALLET);
        assertEq(executor.balancerVault(), BALANCER_VAULT);
        assertEq(executor.hyperLendPool(), HYPERLEND_POOL);
    }

    function test_execute_requires_owner() public {
        TriArbExecutor.Hop[] memory hops = new TriArbExecutor.Hop[](3);
        vm.prank(address(0xBAD));
        vm.expectRevert("SkuaBase: only owner");
        executor.executeTriArb(address(0x1), 1000, hops, 1);
    }

    function test_execute_rejects_zero_flash_asset() public {
        TriArbExecutor.Hop[] memory hops = new TriArbExecutor.Hop[](3);
        vm.prank(OWNER);
        vm.expectRevert("flashAsset zero");
        executor.executeTriArb(address(0), 1000, hops, 1);
    }

    function test_execute_rejects_zero_amount() public {
        TriArbExecutor.Hop[] memory hops = new TriArbExecutor.Hop[](3);
        vm.prank(OWNER);
        vm.expectRevert("flashAmount zero");
        executor.executeTriArb(address(0x1), 0, hops, 1);
    }

    function test_execute_rejects_wrong_hop_count() public {
        TriArbExecutor.Hop[] memory hops = new TriArbExecutor.Hop[](2); // needs 3
        vm.prank(OWNER);
        vm.expectRevert("must be exactly 3 hops");
        executor.executeTriArb(address(0x1), 1000, hops, 1);
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
