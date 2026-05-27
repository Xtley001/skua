// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console2}       from "forge-std/Test.sol";
import {LiquidationExecutor}  from "../src/LiquidationExecutor.sol";

/// Minimal mock HyperLend pool
contract MockHyperLend {
    uint128 public flashFee = 4; // 0.04% in bps

    // Flash loan: transfer tokens to receiver, then call executeOperation
    function flashLoan(
        address receiverAddress,
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata modes,
        address onBehalfOf,
        bytes calldata params,
        uint16 referralCode
    ) external {
        // Transfer requested tokens to receiver (assumes this mock holds them)
        for (uint i = 0; i < assets.length; i++) {
            MockERC20(assets[i]).transfer(receiverAddress, amounts[i]);
        }

        // Compute premiums
        uint256[] memory premiums = new uint256[](amounts.length);
        for (uint i = 0; i < amounts.length; i++) {
            premiums[i] = amounts[i] * flashFee / 10_000;
        }

        // Callback
        LiquidationExecutor(receiverAddress).executeOperation(
            assets, amounts, premiums, receiverAddress, params
        );

        // Pull repayment (simplified — real Aave pulls automatically)
        for (uint i = 0; i < assets.length; i++) {
            uint256 repay = amounts[i] + premiums[i];
            MockERC20(assets[i]).transferFrom(receiverAddress, address(this), repay);
        }
    }

    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool receiveAToken
    ) external {
        // Award collateral + 5% bonus to caller
        uint256 bonus = debtToCover * 105 / 100;
        MockERC20(collateralAsset).transfer(msg.sender, bonus);
        // Burn debt from receiver (simplified)
    }

    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128) {
        return flashFee;
    }

    function getReserveConfigurationData(address) external pure returns (
        uint256, uint256, uint256 liquidationThreshold,
        uint256 liquidationBonus, uint256, bool, bool, bool, bool, bool
    ) {
        return (18, 8000, 8500, 10500, 1000, true, true, false, true, false);
    }
}

/// Minimal mock ERC-20
contract MockERC20 {
    string public symbol;
    uint8  public decimals = 6;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory _sym) { symbol = _sym; }

    function mint(address to, uint256 amount) external { balanceOf[to] += amount; }

    function approve(address s, uint256 a) external returns (bool) {
        allowance[msg.sender][s] = a; return true;
    }

    function transfer(address to, uint256 a) external returns (bool) {
        require(balanceOf[msg.sender] >= a);
        balanceOf[msg.sender] -= a; balanceOf[to] += a; return true;
    }

    function transferFrom(address f, address t, uint256 a) external returns (bool) {
        require(balanceOf[f] >= a);
        require(allowance[f][msg.sender] >= a);
        allowance[f][msg.sender] -= a;
        balanceOf[f] -= a; balanceOf[t] += a; return true;
    }
}

contract LiquidationExecutorTest is Test {
    LiquidationExecutor executor;
    MockHyperLend       hyperlend;
    MockERC20           usdc;
    MockERC20           weth;

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant BALANCER_VAULT = address(0xBAL);
    address constant DEX_ROUTER     = address(0xDEX);
    address constant OWNER          = address(0xABCD);

    function setUp() public {
        hyperlend = new MockHyperLend();
        usdc      = new MockERC20("USDC");
        weth      = new MockERC20("WETH");

        vm.prank(OWNER);
        executor = new LiquidationExecutor(
            PROFIT_WALLET,
            BALANCER_VAULT,
            address(hyperlend),
            DEX_ROUTER
        );

        // Fund HyperLend mock with debt asset (USDC) for flash loan
        usdc.mint(address(hyperlend), 1_000_000e6);
        // Fund HyperLend with collateral (WETH) to award on liquidation
        weth.mint(address(hyperlend), 1_000e18);
    }

    // ── Constructor validation ────────────────────────────────────────────

    function test_constructor_stores_dex_router() public view {
        assertEq(executor.dexRouter(), DEX_ROUTER);
    }

    function test_rejects_zero_dex_router() public {
        vm.prank(OWNER);
        vm.expectRevert("dexRouter zero");
        new LiquidationExecutor(PROFIT_WALLET, BALANCER_VAULT, address(hyperlend), address(0));
    }

    // ── Access control ────────────────────────────────────────────────────

    function test_execute_liquidation_requires_owner() public {
        vm.prank(address(0xDEAD));
        vm.expectRevert("SkuaBase: only owner");
        executor.executeLiquidation(
            address(usdc), address(weth), address(0x1), 1000e6, 0, 0
        );
    }

    function test_execute_liquidation_rejects_zero_debt_asset() public {
        vm.prank(OWNER);
        vm.expectRevert("debtAsset zero");
        executor.executeLiquidation(
            address(0), address(weth), address(0x1), 1000e6, 0, 0
        );
    }

    function test_execute_liquidation_rejects_zero_borrower() public {
        vm.prank(OWNER);
        vm.expectRevert("borrower zero");
        executor.executeLiquidation(
            address(usdc), address(weth), address(0), 1000e6, 0, 0
        );
    }

    function test_execute_liquidation_rejects_zero_amount() public {
        vm.prank(OWNER);
        vm.expectRevert("debtAmount zero");
        executor.executeLiquidation(
            address(usdc), address(weth), address(0x1), 0, 0, 0
        );
    }

    // ── Flash callback auth ───────────────────────────────────────────────

    function test_execute_operation_rejects_wrong_caller() public {
        address[] memory assets  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        uint256[] memory prems   = new uint256[](1);
        assets[0] = address(usdc); amounts[0] = 1000; prems[0] = 0;

        vm.prank(address(0xDEAD));
        vm.expectRevert("executeOperation: wrong caller");
        executor.executeOperation(assets, amounts, prems, address(executor), "");
    }

    function test_execute_operation_rejects_wrong_initiator() public {
        address[] memory assets  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        uint256[] memory prems   = new uint256[](1);
        assets[0] = address(usdc); amounts[0] = 1000; prems[0] = 0;

        vm.prank(address(hyperlend));
        vm.expectRevert("executeOperation: wrong initiator");
        executor.executeOperation(assets, amounts, prems, address(0xBAD), "");
    }

    // ── Integration: end-to-end liquidation ──────────────────────────────
    // NOTE: Full integration test requires a working _swapCollateralToDebt implementation.
    // Placeholder: verify the executor was deployed with correct params.
    function test_deployment_integrity() public view {
        assertEq(executor.owner(),        OWNER);
        assertEq(executor.profitWallet(), PROFIT_WALLET);
        assertEq(executor.balancerVault(), BALANCER_VAULT);
        assertEq(executor.hyperLendPool(), address(hyperlend));
    }
}
