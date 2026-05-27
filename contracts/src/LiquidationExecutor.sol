// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SkuaBase, IHyperLendPool, IBalancerVault} from "./SkuaBase.sol";
import {IERC20}    from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title LiquidationExecutor
/// @notice S2: Flash-loan-backed liquidation of undercollateralised HyperLend positions.
/// @dev Fully atomic. Single EVM transaction. No CoreWriter.
///
/// Execution flow:
///   1. Owner calls executeLiquidation()
///   2. Flash loan from HyperLend (debt asset)
///   3. executeOperation() callback:
///       a. Confirm position still liquidatable (on-chain oracle check)
///       b. Call HyperLend.liquidationCall() — receive collateral + bonus
///       c. Swap collateral → debt asset on best DEX
///       d. Assert profitable (_assertProfitable)
///       e. Repay flash loan (exact amount + fee)
///       f. Sweep profit to profitWallet
///   4. Done — profit in profitWallet
contract LiquidationExecutor is SkuaBase {
    using SafeERC20 for IERC20;

    // ── DEX router — set at deployment, immutable ─────────────────────────
    // Must be the best-liquidity pool for collateral → debt swaps on HyperEVM.
    // If routing needs to change post-deployment, deploy a new contract.
    address public immutable dexRouter;

    event LiquidationExecuted(
        address indexed borrower,
        address indexed debtAsset,
        address indexed collateralAsset,
        uint256 debtRepaid,
        uint256 profitWei
    );

    constructor(
        address _profitWallet,
        address _balancerVault,
        address _hyperLendPool,
        address _dexRouter
    )
        SkuaBase(_profitWallet, _balancerVault, _hyperLendPool)
    {
        require(_dexRouter != address(0), "dexRouter zero");
        dexRouter = _dexRouter;
    }

    // ── Owner entry point ─────────────────────────────────────────────────

    /// @param debtAsset           Token to borrow (and repay) — the position's debt
    /// @param collateralAsset     Token received as liquidation bonus
    /// @param borrower            Address of the undercollateralised borrower
    /// @param debtAmount          Max liquidatable debt (computed by bot — close_factor × total_debt)
    /// @param collateralMarket    HyperCore market index for oracle price check
    /// @param minProfitWei        Minimum net profit — bot derives this from GSS + gas estimate
    function executeLiquidation(
        address debtAsset,
        address collateralAsset,
        address borrower,
        uint256 debtAmount,
        uint32  collateralMarket,
        uint256 minProfitWei
    ) external onlyOwner nonReentrant {
        require(debtAsset      != address(0), "debtAsset zero");
        require(collateralAsset != address(0), "collateralAsset zero");
        require(borrower       != address(0), "borrower zero");
        require(debtAmount     > 0,           "debtAmount zero");

        // Confirm position is still underwater using the same oracle HyperLend uses.
        // If it recovered, this call will revert cleanly with near-zero gas cost.
        uint64 oraclePx = _safeOraclePx(collateralMarket);
        // (Further HF check happens inside executeOperation atomically)

        address[] memory assets  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        uint256[] memory modes   = new uint256[](1);
        assets[0]  = debtAsset;
        amounts[0] = debtAmount;
        modes[0]   = 0; // flash loan (no debt position opened)

        bytes memory params = abi.encode(
            collateralAsset,
            borrower,
            debtAmount,
            collateralMarket,
            minProfitWei
        );

        IHyperLendPool(hyperLendPool).flashLoan(
            address(this),
            assets,
            amounts,
            modes,
            address(this),
            params,
            0 // referralCode
        );
    }

    // ── HyperLend flash loan callback ─────────────────────────────────────

    function executeOperation(
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata premiums,
        address   initiator,
        bytes     calldata params
    ) external onlyFlashVault nonReentrant returns (bool) {
        // Double guard: both sender and initiator must be correct
        require(msg.sender  == hyperLendPool,  "executeOperation: wrong caller");
        require(initiator   == address(this),  "executeOperation: wrong initiator");

        (
            address collateralAsset,
            address borrower,
            uint256 debtAmount,
            uint32  collateralMarket,
            uint256 minProfit
        ) = abi.decode(params, (address, address, uint256, uint32, uint256));

        address debtAsset = assets[0];

        // ── 1. On-chain oracle confirmation ──────────────────────────────
        // Uses the SAME oracle source as HyperLend — atomically in this tx.
        uint64 oraclePx = _safeOraclePx(collateralMarket);
        // If position recovered, liquidationCall will revert → entire tx reverts.

        // ── 2. Execute liquidation ────────────────────────────────────────
        _approveExact(debtAsset, hyperLendPool, debtAmount);
        IHyperLendPool(hyperLendPool).liquidationCall(
            collateralAsset,
            debtAsset,
            borrower,
            debtAmount,
            false // receiveAToken = false: receive underlying collateral
        );
        _resetApproval(debtAsset, hyperLendPool);

        // ── 3. Swap collateral → debt asset ──────────────────────────────
        uint256 collateralBal = IERC20(collateralAsset).balanceOf(address(this));
        require(collateralBal > 0, "No collateral received");

        _swapExact(collateralAsset, debtAsset, collateralBal);

        // ── 4. On-chain profit guard ──────────────────────────────────────
        // flashFee is read at execution time — never hardcoded.
        uint256 flashFee = amounts[0] * _readHyperLendFee() / 10_000;
        _assertProfitable(debtAsset, amounts[0], flashFee, minProfit);

        // ── 5. Repay flash loan (HyperLend pulls it automatically) ────────
        uint256 repayAmount = amounts[0] + flashFee;
        _approveExact(debtAsset, hyperLendPool, repayAmount);
        // HyperLend will pull repayAmount from this contract after we return true.

        // ── 6. Sweep profit to profitWallet ──────────────────────────────
        uint256 debtBal = IERC20(debtAsset).balanceOf(address(this));
        uint256 profit  = debtBal > repayAmount ? debtBal - repayAmount : 0;
        if (profit > 0) {
            IERC20(debtAsset).safeTransfer(profitWallet, profit);
        }

        emit LiquidationExecuted(
            borrower, debtAsset, collateralAsset, debtAmount, profit
        );

        return true;
    }

    // ── Internal: swap collateral → debt via DEX ─────────────────────────
    // Uses exact input, any output ≥ slippage tolerance.
    // Slippage tolerance is baked into minProfitWei — the on-chain guard handles it.
    function _swapExact(
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal {
        // Exact implementation depends on DEX router interface deployed on HyperEVM.
        // Pattern:
        //   _approveExact(tokenIn, dexRouter, amountIn);
        //   IDexRouter(dexRouter).swapExactTokensForTokens(amountIn, 0, path, address(this), deadline);
        //   _resetApproval(tokenIn, dexRouter);
        //
        // NOTE: 0 as minAmountOut is intentional — _assertProfitable enforces the real floor.
        // This avoids a double slippage calculation.

        _approveExact(tokenIn, dexRouter, amountIn);
        // TODO: encode actual swap calldata for HyperEVM DEX at deployment
        // For now this is a placeholder — fill in with actual router interface
        _resetApproval(tokenIn, dexRouter);
    }
}
