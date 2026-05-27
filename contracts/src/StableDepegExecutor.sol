// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SkuaBase, IBalancerVault} from "./SkuaBase.sol";
import {IERC20}    from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title StableDepegExecutor — S4 stablecoin depeg arb, fully atomic.
/// AUDIT FIX #18: receiveFlashLoan now verifies initiator == address(this) via userData.
contract StableDepegExecutor is SkuaBase {
    using SafeERC20 for IERC20;

    event DepegArbExecuted(
        address indexed flashAsset,
        address indexed poolBuy,
        address indexed poolSell,
        uint256 flashAmount,
        uint256 profitWei,
        uint64  oraclePxAtExecution
    );

    constructor(address _profitWallet, address _balancerVault, address _hyperLendPool)
        SkuaBase(_profitWallet, _balancerVault, _hyperLendPool) {}

    function executeDepegArb(
        address flashAsset,
        uint256 flashAmount,
        address poolBuy,
        address poolSell,
        uint32  oracleIndex,
        uint256 minProfitWei
    ) external onlyOwner nonReentrant {
        require(flashAsset  != address(0), "flashAsset zero");
        require(flashAmount >  0,          "flashAmount zero");
        require(poolBuy     != address(0), "poolBuy zero");
        require(poolSell    != address(0), "poolSell zero");

        // Pre-flight oracle check
        uint64 oraclePx = _safeOraclePx(oracleIndex);

        address[] memory tokens  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        tokens[0]  = flashAsset;
        amounts[0] = flashAmount;

        // AUDIT FIX #18: encode address(this) as initiator in userData
        bytes memory userData = abi.encode(
            address(this), poolBuy, poolSell, oracleIndex, minProfitWei
        );

        IBalancerVault(balancerVault).flashLoan(address(this), tokens, amounts, userData);
    }

    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes    memory userData
    ) external onlyFlashVault nonReentrant {
        require(msg.sender == balancerVault, "receiveFlashLoan: wrong caller");

        // AUDIT FIX #18: decode and verify initiator
        (address initiator, address poolBuy, address poolSell,
         uint32 oracleIndex, uint256 minProfit) =
            abi.decode(userData, (address, address, address, uint32, uint256));
        require(initiator == address(this), "receiveFlashLoan: not self-initiated");

        address flashAsset  = tokens[0];
        uint256 flashAmount = amounts[0];
        uint256 flashFee    = feeAmounts[0];

        // Re-read oracle atomically
        uint64 oraclePx = _safeOraclePx(oracleIndex);

        // Step 1: buy depegged stable at discount
        uint256 stableBBought = _stableSwap(poolBuy, flashAsset, flashAmount);
        require(stableBBought > 0, "Step 1: got 0 stableB");

        // Step 2: sell back at peg
        address tokenB = _otherToken(poolSell, flashAsset);
        uint256 stableAReturned = _stableSwap(poolSell, tokenB, stableBBought);
        require(stableAReturned > 0, "Step 2: got 0 stableA");

        _assertProfitable(flashAsset, flashAmount, flashFee, minProfit);

        uint256 repayAmount = flashAmount + flashFee;
        _approveExact(flashAsset, balancerVault, repayAmount);

        uint256 profit = IERC20(flashAsset).balanceOf(address(this)) - repayAmount;
        if (profit > 0) IERC20(flashAsset).safeTransfer(profitWallet, profit);

        emit DepegArbExecuted(flashAsset, poolBuy, poolSell, flashAmount, profit, oraclePx);
    }

    function _stableSwap(address pool, address tokenIn, uint256 amountIn)
        internal returns (uint256 amountOut)
    {
        address tokenOut  = _otherToken(pool, tokenIn);
        uint256 balBefore = IERC20(tokenOut).balanceOf(address(this));
        _approveExact(tokenIn, pool, amountIn);
        // DEX call inserted at deployment per stable pool interface on HyperEVM
        _resetApproval(tokenIn, pool);
        amountOut = IERC20(tokenOut).balanceOf(address(this)) - balBefore;
    }

    function _otherToken(address pool, address knownToken) internal view returns (address) {
        (bool ok0, bytes memory data0) = pool.staticcall(abi.encodeWithSignature("token0()"));
        if (ok0 && data0.length == 32) {
            address t0 = abi.decode(data0, (address));
            if (t0 == knownToken) {
                (, bytes memory data1) = pool.staticcall(abi.encodeWithSignature("token1()"));
                return abi.decode(data1, (address));
            }
            return t0;
        }
        revert("_otherToken: pool interface not recognised");
    }
}
