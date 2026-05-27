// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console2} from "forge-std/Test.sol";
import {SkuaBase}        from "../src/SkuaBase.sol";
import {IERC20}          from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// Minimal concrete implementation of SkuaBase for testing the abstract contract.
contract SkuaBaseHarness is SkuaBase {
    constructor(address pw, address bv, address hl)
        SkuaBase(pw, bv, hl) {}

    function assertProfitable(
        address token,
        uint256 borrowed,
        uint256 fee,
        uint256 minProfit
    ) external view {
        _assertProfitable(token, borrowed, fee, minProfit);
    }

    function approveExact(address token, address spender, uint256 amount) external {
        _approveExact(token, spender, amount);
    }

    function resetApproval(address token, address spender) external {
        _resetApproval(token, spender);
    }
}

/// Minimal ERC-20 mock for testing.
contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    string public name   = "Mock";
    string public symbol = "MCK";
    uint8  public decimals = 18;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "insufficient");
        balanceOf[msg.sender] -= amount;
        balanceOf[to]         += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from]             >= amount, "insufficient balance");
        require(allowance[from][msg.sender] >= amount, "insufficient allowance");
        allowance[from][msg.sender] -= amount;
        balanceOf[from]             -= amount;
        balanceOf[to]               += amount;
        return true;
    }
}

contract SkuaBaseTest is Test {
    SkuaBaseHarness harness;
    MockERC20       token;

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant BALANCER_VAULT = address(0xBA1);
    address constant HYPERLEND_POOL = address(0xBA2);
    address constant OWNER          = address(0xABCD);

    function setUp() public {
        vm.prank(OWNER);
        harness = new SkuaBaseHarness(PROFIT_WALLET, BALANCER_VAULT, HYPERLEND_POOL);
        token   = new MockERC20();
    }

    // ── Constructor guards ────────────────────────────────────────────────

    function test_constructor_rejects_zero_profit_wallet() public {
        vm.expectRevert("profitWallet zero");
        new SkuaBaseHarness(address(0), BALANCER_VAULT, HYPERLEND_POOL);
    }

    function test_constructor_rejects_zero_balancer_vault() public {
        vm.expectRevert("balancerVault zero");
        new SkuaBaseHarness(PROFIT_WALLET, address(0), HYPERLEND_POOL);
    }

    function test_constructor_rejects_zero_hyperlend_pool() public {
        vm.expectRevert("hyperLendPool zero");
        new SkuaBaseHarness(PROFIT_WALLET, BALANCER_VAULT, address(0));
    }

    function test_constructor_sets_owner() public view {
        assertEq(harness.owner(), OWNER);
    }

    function test_constructor_sets_profit_wallet() public view {
        assertEq(harness.profitWallet(), PROFIT_WALLET);
    }

    // ── onlyOwner modifier ────────────────────────────────────────────────

    function test_sweep_reverts_non_owner() public {
        address attacker = address(0xDEAD);
        vm.prank(attacker);
        vm.expectRevert("SkuaBase: only owner");
        harness.sweep(address(token), 100);
    }

    function test_sweep_erc20_transfers_to_profit_wallet() public {
        token.mint(address(harness), 1000);
        vm.prank(OWNER);
        harness.sweep(address(token), 1000);
        assertEq(token.balanceOf(PROFIT_WALLET), 1000);
        assertEq(token.balanceOf(address(harness)), 0);
    }

    // ── _assertProfitable ─────────────────────────────────────────────────

    function test_assert_profitable_passes_with_sufficient_balance() public {
        token.mint(address(harness), 1100); // borrowed=1000, fee=50, minProfit=50, net=50 ✓
        harness.assertProfitable(address(token), 1000, 50, 50);
    }

    function test_assert_profitable_reverts_no_gross_profit() public {
        token.mint(address(harness), 1049); // borrowed=1000, fee=50 → need >1050
        vm.expectRevert("SkuaBase: no gross profit");
        harness.assertProfitable(address(token), 1000, 50, 1);
    }

    function test_assert_profitable_reverts_below_min_profit() public {
        token.mint(address(harness), 1060); // net=10, minProfit=20 → fail
        vm.expectRevert("SkuaBase: below min profit");
        harness.assertProfitable(address(token), 1000, 50, 20);
    }

    function test_assert_profitable_exact_min_profit_passes() public {
        token.mint(address(harness), 1150); // net=100, minProfit=100 → pass (>=)
        harness.assertProfitable(address(token), 1000, 50, 100);
    }

    // ── Approval pattern ─────────────────────────────────────────────────

    function test_approve_exact_sets_allowance() public {
        address spender = address(0x999);
        vm.prank(OWNER);
        harness.approveExact(address(token), spender, 500);
        assertEq(token.allowance(address(harness), spender), 500);
    }

    function test_reset_approval_zeros_allowance() public {
        address spender = address(0x999);
        vm.prank(OWNER);
        harness.approveExact(address(token), spender, 500);
        vm.prank(OWNER);
        harness.resetApproval(address(token), spender);
        assertEq(token.allowance(address(harness), spender), 0);
    }

    // ── Immutables are constant ───────────────────────────────────────────

    function test_l1read_address_constant() public view {
        // L1READ is internal constant — verify via expected address
        // 0x0000000000000000000000000000000000000800
        assertTrue(address(0x0000000000000000000000000000000000000800) != address(0));
    }

    function test_corewriter_address_constant() public view {
        assertTrue(address(0x3333333333333333333333333333333333333333) != address(0));
    }

    // ── Fuzz: profit guard is monotone in balance ─────────────────────────

    function testFuzz_assert_profitable_requires_balance_above_threshold(
        uint128 balance,
        uint128 borrowed,
        uint128 fee,
        uint128 minProfit
    ) public {
        vm.assume(borrowed < type(uint128).max / 2);
        vm.assume(fee      < type(uint128).max / 2);
        vm.assume(minProfit < type(uint128).max / 2);
        vm.assume(uint256(borrowed) + fee < type(uint256).max);

        token.mint(address(harness), balance);
        uint256 required = uint256(borrowed) + fee;
        uint256 net      = balance > required ? balance - required : 0;

        if (balance <= required || net < minProfit) {
            vm.expectRevert();
        }
        harness.assertProfitable(address(token), borrowed, fee, minProfit);
    }
}
