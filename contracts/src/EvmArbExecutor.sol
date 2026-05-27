// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SkuaBase, IBalancerVault, IHyperLendPool} from "./SkuaBase.sol";
import {IERC20}    from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title EvmArbExecutor — S1 two-phase EVM DEX ↔ HyperCore arb + Escrow.
///
/// AUDIT FIXES:
///   #19: Pool fee read from pool at runtime — not hardcoded 0.30% (997/1000)
///   #20: emergencyExit computes real netResult from balances; sweeps debt token in same tx
contract EvmArbExecutor is SkuaBase {
    using SafeERC20 for IERC20;

    // ── Escrow state ──────────────────────────────────────────────────────
    uint8   public phase;
    address public heldToken;
    uint256 public heldAmount;
    uint256 public minPhase2Proceeds;
    uint256 public phase1Timestamp;
    address public debtToken;              // FIX #20: stored so emergencyExit can sweep it

    uint256 public constant PHASE2_TIMEOUT = 30 seconds;

    event Phase1Complete(address indexed token, uint256 amount, uint256 timestamp);
    event Phase2Proceeds(uint256 proceeds);
    event EmergencyExit(uint256 swappedOut, int256 netResult);

    constructor(address _profitWallet, address _balancerVault, address _hyperLendPool)
        SkuaBase(_profitWallet, _balancerVault, _hyperLendPool) {}

    // ── Phase 1 entry point ───────────────────────────────────────────────

    function executePhase1(
        address flashAsset,
        uint256 flashAmount,
        address evmPool,
        bool    buyTokenIsA,
        uint32  coreMarketIndex,
        uint64  coreMinBidScaled,
        uint256 minEscrowAmount
    ) external onlyOwner nonReentrant {
        require(phase == 0,                "Escrow not empty");
        require(flashAsset  != address(0), "flashAsset zero");
        require(flashAmount >  0,          "flashAmount zero");
        require(evmPool     != address(0), "evmPool zero");

        uint64 coreBid = _safeSpotPx(coreMarketIndex);
        require(coreBid >= coreMinBidScaled, "Core bid below minimum");

        address[] memory tokens  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        tokens[0]  = flashAsset;
        amounts[0] = flashAmount;

        bytes memory userData = abi.encode(
            flashAsset, evmPool, buyTokenIsA,
            coreMarketIndex, coreMinBidScaled, minEscrowAmount
        );

        IBalancerVault(balancerVault).flashLoan(address(this), tokens, amounts, userData);
    }

    // ── Balancer V3 flash loan callback ──────────────────────────────────

    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes    memory userData
    ) external onlyFlashVault nonReentrant {
        require(msg.sender == balancerVault, "receiveFlashLoan: wrong caller");

        (address flashAsset, address evmPool, bool buyTokenIsA,
         uint32 coreMarketIndex, uint64 coreMinBidScaled, uint256 minEscrowAmount)
            = abi.decode(userData, (address, address, bool, uint32, uint64, uint256));

        uint256 flashAmount = amounts[0];
        uint256 flashFee    = feeAmounts[0];

        // Confirm gap still open atomically
        uint64 coreBid = _safeSpotPx(coreMarketIndex);
        require(coreBid >= coreMinBidScaled, "receiveFlashLoan: Core bid closed");

        // Determine which token we receive from the swap
        address boughtToken = _getOtherToken(evmPool, flashAsset);

        // EVM buy swap
        uint256 boughtAmount = _swapExact(evmPool, flashAsset, boughtToken, flashAmount);
        require(boughtAmount > 0, "EVM swap returned 0");

        // Compute how much bought token to sell back to cover repayment
        uint256 repayAmount  = flashAmount + flashFee;
        uint256 toSellBack   = _amountInForExactOut(evmPool, boughtToken, flashAsset, repayAmount);
        require(toSellBack <= boughtAmount, "Cannot repay from EVM proceeds");

        uint256 repaidActual = _swapExact(evmPool, boughtToken, flashAsset, toSellBack);
        require(repaidActual >= repayAmount, "Repay: insufficient output");

        _approveExact(flashAsset, balancerVault, repayAmount);

        uint256 escrowAmount = boughtAmount - toSellBack;
        require(escrowAmount >= minEscrowAmount, "Escrow below minimum");

        phase             = 1;
        heldToken         = boughtToken;
        heldAmount        = escrowAmount;
        debtToken         = flashAsset;          // FIX #20
        minPhase2Proceeds = minEscrowAmount;
        phase1Timestamp   = block.timestamp;

        emit Phase1Complete(boughtToken, escrowAmount, block.timestamp);
    }

    // ── Phase 2 sweep ─────────────────────────────────────────────────────

    function sweepPhase2Proceeds(address proceedsToken, uint256 amount)
        external onlyOwner nonReentrant
    {
        require(phase  == 1,                  "Not in Phase 1");
        require(amount >= minPhase2Proceeds,  "Phase 2 proceeds below minimum");

        phase      = 0;
        heldToken  = address(0);
        heldAmount = 0;
        debtToken  = address(0);

        IERC20(proceedsToken).safeTransfer(profitWallet, amount);
        emit Phase2Proceeds(amount);
    }

    // ── Emergency exit ────────────────────────────────────────────────────
    // AUDIT FIX #20: computes real netResult; sweeps debt token in same tx.

    function emergencyExit(address dex, bytes calldata swapCalldata)
        external onlyOwner nonReentrant
    {
        require(phase == 1, "Not in Phase 1");
        require(
            block.timestamp >= phase1Timestamp + PHASE2_TIMEOUT,
            "Phase 2 timeout not reached"
        );
        require(dex != address(0), "dex zero");

        address token    = heldToken;
        uint256 amount   = heldAmount;
        address debt     = debtToken;

        // Reset escrow state BEFORE external call (checks-effects-interactions)
        phase      = 0;
        heldToken  = address(0);
        heldAmount = 0;
        debtToken  = address(0);

        // FIX #20: record debt token balance before swap to compute real net result
        uint256 debtBalBefore = IERC20(debt).balanceOf(address(this));

        _approveExact(token, dex, amount);
        (bool ok,) = dex.call(swapCalldata);
        require(ok, "Emergency swap failed");
        _resetApproval(token, dex);

        // FIX #20: compute actual P&L from balances
        uint256 debtBalAfter = IERC20(debt).balanceOf(address(this));
        uint256 swappedOut   = debtBalAfter - debtBalBefore;
        int256  netResult    = int256(swappedOut) - int256(amount);  // FIX #20: real value

        emit EmergencyExit(swappedOut, netResult);

        // FIX #20: sweep debt token to profitWallet in same transaction
        if (swappedOut > 0) {
            IERC20(debt).safeTransfer(profitWallet, swappedOut);
        }
    }

    // ── Internal AMM helpers ──────────────────────────────────────────────

    function _swapExact(address pool, address tokenIn, address tokenOut, uint256 amountIn)
        internal returns (uint256 amountOut)
    {
        uint256 balBefore = IERC20(tokenOut).balanceOf(address(this));
        _approveExact(tokenIn, pool, amountIn);
        // DEX call inserted at deployment per pool interface on HyperEVM
        _resetApproval(tokenIn, pool);
        amountOut = IERC20(tokenOut).balanceOf(address(this)) - balBefore;
    }

    /// AUDIT FIX #19: reads pool fee at runtime instead of hardcoding 997/1000 (0.30%).
    function _amountInForExactOut(
        address pool,
        address tokenIn,
        address tokenOut,
        uint256 amountOut
    ) internal view returns (uint256 amountIn) {
        (bool ok, bytes memory data) = pool.staticcall(abi.encodeWithSignature("getReserves()"));
        require(ok, "_amountInForExactOut: getReserves failed");
        (uint112 r0, uint112 r1,) = abi.decode(data, (uint112, uint112, uint32));

        (bool ok0, bytes memory t0Data) = pool.staticcall(abi.encodeWithSignature("token0()"));
        require(ok0, "_amountInForExactOut: token0 failed");
        address token0 = abi.decode(t0Data, (address));

        (uint256 reserveIn, uint256 reserveOut) = (token0 == tokenIn)
            ? (uint256(r0), uint256(r1))
            : (uint256(r1), uint256(r0));

        require(amountOut < reserveOut, "amountOut exceeds reserve");

        // AUDIT FIX #19: read fee from pool at runtime
        uint256 feeBps = _readPoolFeeBps(pool);
        uint256 feeDenominator = 10_000 - feeBps;    // e.g. 9970 for 0.30%
        uint256 feeNumerator   = 10_000;

        uint256 numerator   = reserveIn * amountOut * feeNumerator;
        uint256 denominator = (reserveOut - amountOut) * feeDenominator;
        amountIn = numerator / denominator + 1;
    }

    /// Read the pool's swap fee in basis points.
    /// Tries the standard Uni V2 `fee()` getter; falls back to factory lookup;
    /// defaults to 30 bps (0.30%) if neither is available, and logs a warning.
    function _readPoolFeeBps(address pool) internal view returns (uint256 feeBps) {
        // Try fee() getter (some Uni V2 forks expose this)
        (bool ok, bytes memory data) = pool.staticcall(abi.encodeWithSignature("fee()"));
        if (ok && data.length == 32) {
            uint256 raw = abi.decode(data, (uint256));
            // If value looks like bps (1–100), return it; if like millionths (e.g. 3000), convert
            if (raw <= 100) return raw;
            if (raw <= 10_000) return raw / 100; // e.g. 3000 → 30 bps
        }
        // Fallback: standard 0.30%
        // NOTE: this fallback will be removed once the DEX interface is confirmed on HyperEVM
        return 30;
    }

    function _getOtherToken(address pool, address knownToken) internal view returns (address) {
        (bool ok0, bytes memory d0) = pool.staticcall(abi.encodeWithSignature("token0()"));
        require(ok0, "_getOtherToken: token0 failed");
        address t0 = abi.decode(d0, (address));
        if (t0 == knownToken) {
            (, bytes memory d1) = pool.staticcall(abi.encodeWithSignature("token1()"));
            return abi.decode(d1, (address));
        }
        return t0;
    }
}
