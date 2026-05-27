// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SkuaBase, IBalancerVault} from "./SkuaBase.sol";
import {IERC20}    from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title TriArbExecutor — S3 triangular arb, fully atomic.
contract TriArbExecutor is SkuaBase {
    using SafeERC20 for IERC20;

    struct Hop {
        address pool;
        address tokenIn;
        address tokenOut;
    }

    event TriArbExecuted(
        address indexed startToken,
        uint256 flashAmount,
        uint256 profitWei
    );

    constructor(address _profitWallet, address _balancerVault, address _hyperLendPool)
        SkuaBase(_profitWallet, _balancerVault, _hyperLendPool) {}

    function executeTriArb(
        address       flashAsset,
        uint256       flashAmount,
        Hop[] calldata hops,
        uint256       minProfitWei
    ) external onlyOwner nonReentrant {
        require(flashAsset  != address(0), "flashAsset zero");
        require(flashAmount >  0,          "flashAmount zero");
        require(hops.length == 3,          "must be exactly 3 hops");

        address[] memory tokens  = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        tokens[0]  = flashAsset;
        amounts[0] = flashAmount;

        // AUDIT FIX #18: encode address(this) as initiator in userData
        bytes memory userData = abi.encode(address(this), hops, minProfitWei);

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
        (address initiator, Hop[] memory hops, uint256 minProfit) =
            abi.decode(userData, (address, Hop[], uint256));
        require(initiator == address(this), "receiveFlashLoan: not self-initiated");

        address startToken  = tokens[0];
        uint256 flashAmount = amounts[0];
        uint256 flashFee    = feeAmounts[0]; // from Balancer — never hardcoded

        uint256 amountIn = flashAmount;
        for (uint256 i = 0; i < hops.length; i++) {
            amountIn = _swap(hops[i].pool, hops[i].tokenIn, hops[i].tokenOut, amountIn);
            require(amountIn > 0, "Hop returned 0");
        }

        _assertProfitable(startToken, flashAmount, flashFee, minProfit);

        uint256 repayAmount = flashAmount + flashFee;
        _approveExact(startToken, balancerVault, repayAmount);

        uint256 profit = IERC20(startToken).balanceOf(address(this)) - repayAmount;
        if (profit > 0) IERC20(startToken).safeTransfer(profitWallet, profit);

        emit TriArbExecuted(startToken, flashAmount, profit);
    }

    function _swap(address pool, address tokenIn, address tokenOut, uint256 amountIn)
        internal returns (uint256 amountOut)
    {
        uint256 balBefore = IERC20(tokenOut).balanceOf(address(this));
        _approveExact(tokenIn, pool, amountIn);
        // DEX call inserted at deployment per pool interface on HyperEVM
        // Uni V2-style: IUniV2Pair(pool).swap(amount0Out, amount1Out, address(this), "")
        // Balancer V3: call through vault.swap(SingleSwap{...}, FundManagement{...}, 0, deadline)
        _resetApproval(tokenIn, pool);
        amountOut = IERC20(tokenOut).balanceOf(address(this)) - balBefore;
    }
}
