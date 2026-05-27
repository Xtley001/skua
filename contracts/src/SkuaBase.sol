// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// HyperEVM runs Cancun hardfork.
// Transient storage (EIP-1153) is available — used for reentrancy guard.

import {IERC20}     from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20}  from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

// ── HyperCore precompile interfaces ─────────────────────────────────────────

interface IL1Read {
    function spotPx(uint32 marketIndex) external view returns (uint64);
    function oraclePx(uint32 tokenIndex) external view returns (uint64);
    function spotBalance(address user, uint32 tokenIndex) external view returns (uint64);
}

interface ICoreWriter {
    function placeOrder(
        uint32 marketIndex,
        bool   isBuy,
        uint64 limitPx,
        uint64 sz,
        uint8  tif,
        uint64 cloid
    ) external;
    function spotSend(address to, uint32 tokenIndex, uint64 amount) external;
}

// ── HyperLend / Balancer interfaces (minimal) ───────────────────────────────

interface IHyperLendPool {
    function flashLoan(
        address receiverAddress,
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata modes,
        address onBehalfOf,
        bytes   calldata params,
        uint16  referralCode
    ) external;

    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool    receiveAToken
    ) external;

    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);

    function getReserveConfigurationData(address asset)
        external view
        returns (
            uint256 decimals,
            uint256 ltv,
            uint256 liquidationThreshold,
            uint256 liquidationBonus,
            uint256 reserveFactor,
            bool usageAsCollateralEnabled,
            bool borrowingEnabled,
            bool stableBorrowRateEnabled,
            bool isActive,
            bool isFrozen
        );
}

interface IBalancerVault {
    function flashLoan(
        address recipient,
        address[] memory tokens,
        uint256[] memory amounts,
        bytes   memory userData
    ) external;

    function getProtocolFeePercentages()
        external view
        returns (
            uint256 swapFeePercentage,
            uint256 flashLoanFeePercentage,
            uint256 yieldFeePercentage
        );
}

// ── SkuaBase ─────────────────────────────────────────────────────────────────

abstract contract SkuaBase {
    using SafeERC20 for IERC20;

    // ── System addresses — constants, NEVER mutable ──────────────────────
    // L1Read precompile: 0x000...0800 (immutable on HyperEVM)
    IL1Read internal constant L1READ =
        IL1Read(0x0000000000000000000000000000000000000800);

    // CoreWriter: 0x333...3333 (immutable on HyperEVM)
    ICoreWriter internal constant COREWRITER =
        ICoreWriter(0x3333333333333333333333333333333333333333);

    // ── Immutables — set in constructor, never zero ───────────────────────
    address public immutable owner;
    address public immutable profitWallet;
    address public immutable balancerVault;
    address public immutable hyperLendPool;

    // ── Transient reentrancy guard (EIP-1153, Cancun) ────────────────────
    // Uses tload/tstore — resets automatically at end of transaction.
    bytes32 private constant REENTRANCY_SLOT =
        keccak256("skua.reentrancy.v1");

    modifier nonReentrant() {
        assembly {
            if tload(REENTRANCY_SLOT) { revert(0, 0) }
            tstore(REENTRANCY_SLOT, 1)
        }
        _;
        assembly { tstore(REENTRANCY_SLOT, 0) }
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "SkuaBase: only owner");
        _;
    }

    modifier onlyFlashVault() {
        require(
            msg.sender == balancerVault || msg.sender == hyperLendPool,
            "SkuaBase: only flash vault"
        );
        _;
    }

    constructor(
        address _profitWallet,
        address _balancerVault,
        address _hyperLendPool
    ) {
        require(_profitWallet  != address(0), "profitWallet zero");
        require(_balancerVault != address(0), "balancerVault zero");
        require(_hyperLendPool != address(0), "hyperLendPool zero");
        owner         = msg.sender;
        profitWallet  = _profitWallet;
        balancerVault = _balancerVault;
        hyperLendPool = _hyperLendPool;
    }

    // ── On-chain profit guard ─────────────────────────────────────────────
    // Never remove. Never set minProfit to 0 as a "debug" shortcut.
    // Called inside every flash callback before repayment.
    function _assertProfitable(
        address token,
        uint256 borrowedAmount,
        uint256 flashFee,
        uint256 minProfit
    ) internal view {
        uint256 balance  = IERC20(token).balanceOf(address(this));
        uint256 required = borrowedAmount + flashFee;
        require(balance > required,  "SkuaBase: no gross profit");
        uint256 net = balance - required;
        require(net >= minProfit,    "SkuaBase: below min profit");
    }

    // ── Precompile zero-price guards ─────────────────────────────────────
    // Any zero price from the precompile means we MUST revert — acting on
    // zero prices would produce nonsensical profit calculations on-chain.
    function _safeSpotPx(uint32 marketIndex) internal view returns (uint64 price) {
        price = L1READ.spotPx(marketIndex);
        require(price > 0, "SkuaBase: spotPx returned 0");
    }

    function _safeOraclePx(uint32 tokenIndex) internal view returns (uint64 price) {
        price = L1READ.oraclePx(tokenIndex);
        require(price > 0, "SkuaBase: oraclePx returned 0");
    }

    // ── Exact approval pattern ───────────────────────────────────────────
    // NEVER use type(uint256).max.
    // Reset to 0 first to handle non-standard ERC-20s.
    function _approveExact(address token, address spender, uint256 amount) internal {
        IERC20(token).safeApprove(spender, 0);
        IERC20(token).safeApprove(spender, amount);
    }

    function _resetApproval(address token, address spender) internal {
        IERC20(token).safeApprove(spender, 0);
    }

    // ── Read HyperLend flash fee at execution time ────────────────────────
    // Called inside flash callbacks — never use a hardcoded constant.
    function _readHyperLendFee() internal view returns (uint256) {
        return IHyperLendPool(hyperLendPool).FLASHLOAN_PREMIUM_TOTAL();
    }

    // ── Read Balancer flash fee at execution time ─────────────────────────
    function _readBalancerFee() internal view returns (uint256 flashFee) {
        (, flashFee,) = IBalancerVault(balancerVault).getProtocolFeePercentages();
        // flashFee is WAD (10^18 = 100%). Convert to bps for internal use if needed.
    }

    // ── Emergency sweep — onlyOwner, always to profitWallet ──────────────
    function sweep(address token, uint256 amount) external onlyOwner {
        if (token == address(0)) {
            (bool ok,) = profitWallet.call{value: amount}("");
            require(ok, "ETH sweep failed");
        } else {
            IERC20(token).safeTransfer(profitWallet, amount);
        }
    }

    function sweepAll(address token) external onlyOwner {
        uint256 bal = IERC20(token).balanceOf(address(this));
        if (bal > 0) IERC20(token).safeTransfer(profitWallet, bal);
    }

    // ── Balance introspection — for monitoring ────────────────────────────
    // Contract must hold ZERO tokens between trades.
    // Non-zero balance between trades is a bug indicator.
    function tokenBalance(address token) external view returns (uint256) {
        return IERC20(token).balanceOf(address(this));
    }

    // ── Receive ETH (for gas refunds etc.) ───────────────────────────────
    receive() external payable {}
}
