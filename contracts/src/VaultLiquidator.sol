// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title  VaultLiquidator (Morpho Blue native flow)
/// @notice Capital-free Morpho Blue liquidations. No flash loan is used:
///
///   executeLiquidation
///     -> morpho.liquidate(mp, borrower, seizedAssets, repaidShares, data)
///          Morpho sends the seized collateral to this contract
///          Morpho calls onMorphoLiquidate(repaidAssets, data)
///            swap collateral -> loan token, enforce the profit gate,
///            approve Morpho for repaidAssets
///          Morpho pulls repaidAssets of loan token via transferFrom
///   profit (loan token) stays in this contract; the owner sweeps it with rescue().
///
/// The Morpho callback ordering (collateral out before debt pulled) is what makes
/// the flow capital-free. The session guard ensures exactly one in-flight
/// liquidation and binds the callback to the exact payload this contract built.

struct MarketParams {
    address loanToken;
    address collateralToken;
    address oracle;
    address irm;
    uint256 lltv;
}

interface IMorpho {
    function liquidate(
        MarketParams memory marketParams,
        address borrower,
        uint256 seizedAssets,
        uint256 repaidShares,
        bytes memory data
    ) external returns (uint256 assetsSeized, uint256 assetsRepaid);
}

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
}

/// @dev Uniswap V3 SwapRouter02-style exactInputSingle (no deadline field).
///      Verify the target deployment matches this signature before deploying.
interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

contract VaultLiquidator {
    // ------------------------------------------------------------------
    // Roles
    // ------------------------------------------------------------------

    address public owner;
    address public pendingOwner;
    /// @notice Hot keys that may trigger liquidations. They cannot rescue,
    ///         pause, or reconfigure.
    mapping(address => bool) public operators;

    // ------------------------------------------------------------------
    // Config
    // ------------------------------------------------------------------

    /// @notice The only Morpho instance this contract will liquidate against
    ///         or accept callbacks from.
    address public immutable morpho;
    address public swapRouter;
    /// @notice Default Uniswap V3 pool fee (500 = 0.05%, 3000 = 0.3%, 10000 = 1%).
    uint24 public defaultPoolFee;
    /// @notice Applied as sqrtPriceLimitX96 on every swap; 0 disables the limit.
    uint160 public sqrtPriceLimitX96;
    /// @notice Emergency switch; operators cannot liquidate while paused.
    bool public paused;

    // ------------------------------------------------------------------
    // Session
    // ------------------------------------------------------------------

    /// @dev Commitment to the in-flight payload. Non-zero while a flow is
    ///      mid-execution, zero when idle. Includes a nonce so two identical
    ///      calls back-to-back still produce distinct commitments.
    bytes32 private _session;
    uint256 private _nonce;

    /// @dev Everything the callback needs. All value types, so abi.decode
    ///      is cheap and memory-safe.
    struct CallbackData {
        address borrower;
        address loanToken;
        address collateralToken;
        uint24 poolFee;
        uint256 minNetProfit;    // loan-token units, this trade only
        uint256 loanBalBefore;   // loan-token balance before the liquidation
        uint256 collBalBefore;   // collateral balance before the liquidation
        uint256 nonce;           // unique per call
    }

    // ------------------------------------------------------------------
    // Errors
    // ------------------------------------------------------------------

    error Unauthorized();
    error ZeroAddress();
    error ZeroMinProfit();
    error InvalidLiquidationParams();
    error SessionActive();
    error CallbackNotExecuted();
    error InsufficientProfit(uint256 expected, uint256 actual);
    error SwapFailed();
    error TransferFailed();
    error Paused();

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    event LiquidationExecuted(
        address indexed borrower,
        address indexed loanToken,
        address indexed collateralToken,
        uint256 debtRepaid,
        uint256 profit
    );
    event SwapRouterUpdated(address indexed router);
    event PoolFeeUpdated(uint24 fee);
    event PriceLimitUpdated(uint160 sqrtPriceLimitX96);
    event OperatorUpdated(address indexed operator, bool allowed);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    // ------------------------------------------------------------------
    // Modifiers
    // ------------------------------------------------------------------

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier onlyOperator() {
        if (msg.sender != owner && !operators[msg.sender]) revert Unauthorized();
        _;
    }

    modifier whenNotPaused() {
        if (paused) revert Paused();
        _;
    }

    constructor(address morpho_, address router_, uint24 defaultPoolFee_) {
        if (morpho_ == address(0) || router_ == address(0)) revert ZeroAddress();
        owner = msg.sender;
        morpho = morpho_;
        swapRouter = router_;
        defaultPoolFee = defaultPoolFee_;
    }

    // ------------------------------------------------------------------
    // Entry point
    // ------------------------------------------------------------------

    /// @notice Liquidate `borrower` on Morpho. Exactly one of seizedAssets /
    ///         repaidShares must be non-zero (Morpho's own rule).
    /// @param minNetProfit Minimum profit in loan-token units for THIS trade
    ///        (balance delta; pre-existing balances do not count). Must be > 0,
    ///        in the loan token's decimals.
    /// @param poolFee Uniswap V3 fee tier for the collateral -> loan swap;
    ///        0 = defaultPoolFee.
    function executeLiquidation(
        MarketParams calldata mp,
        address borrower,
        uint256 seizedAssets,
        uint256 repaidShares,
        uint256 minNetProfit,
        uint24 poolFee
    ) external onlyOperator whenNotPaused {
        if (_session != bytes32(0)) revert SessionActive();
        if (minNetProfit == 0) revert ZeroMinProfit();
        if ((seizedAssets == 0) == (repaidShares == 0)) revert InvalidLiquidationParams();
        if (mp.loanToken == mp.collateralToken) revert InvalidLiquidationParams();

        bytes memory data = abi.encode(
            CallbackData({
                borrower: borrower,
                loanToken: mp.loanToken,
                collateralToken: mp.collateralToken,
                poolFee: poolFee == 0 ? defaultPoolFee : poolFee,
                minNetProfit: minNetProfit,
                loanBalBefore: IERC20(mp.loanToken).balanceOf(address(this)),
                collBalBefore: IERC20(mp.collateralToken).balanceOf(address(this)),
                nonce: ++_nonce
            })
        );
        _session = keccak256(data);

        // `data` is non-empty, so Morpho calls onMorphoLiquidate before pulling repayment.
        IMorpho(morpho).liquidate(mp, borrower, seizedAssets, repaidShares, data);

        // The callback clears the session. If it is still set, the gate never ran.
        if (_session != bytes32(0)) revert CallbackNotExecuted();
    }

    // ------------------------------------------------------------------
    // Morpho callback
    // ------------------------------------------------------------------

    /// @notice Called by Morpho after it has sent us the seized collateral and
    ///         before it pulls `repaidAssets` of loan token from us.
    /// @dev Accepts only Morpho, only during a session this contract opened,
    ///      and only with the exact payload it built.
    function onMorphoLiquidate(uint256 repaidAssets, bytes calldata data) external {
        if (msg.sender != morpho || _session == bytes32(0) || keccak256(data) != _session) {
            revert Unauthorized();
        }
        _session = bytes32(0); // single use

        CallbackData memory cb = abi.decode(data, (CallbackData));

        uint256 profit = _swapAndGate(cb, repaidAssets);

        // Morpho pulls exactly repaidAssets after we return; the surplus stays here.
        _safeApprove(cb.loanToken, msg.sender, repaidAssets);

        emit LiquidationExecuted(cb.borrower, cb.loanToken, cb.collateralToken, repaidAssets, profit);
    }

    // ------------------------------------------------------------------
    // Shared core (reusable by any future callback path)
    // ------------------------------------------------------------------

    /// @dev Swaps the collateral received since `cb.collBalBefore` into the loan
    ///      token and enforces: balance >= loanBalBefore + owed + minNetProfit.
    ///      `owed` is what the funding source takes back (Morpho: repaidAssets;
    ///      a flash provider: amount + fee).
    /// @return profit loan-token surplus over (loanBalBefore + owed).
    function _swapAndGate(CallbackData memory cb, uint256 owed) internal returns (uint256 profit) {
        uint256 base = cb.loanBalBefore + owed;
        uint256 seized = IERC20(cb.collateralToken).balanceOf(address(this)) - cb.collBalBefore;

        if (seized > 0) {
            // Derive the swap's minimum output from the profit target, so a poor
            // fill (sandwich, thin pool) reverts inside the swap instead of leaking.
            uint256 target = base + cb.minNetProfit;
            uint256 cur = IERC20(cb.loanToken).balanceOf(address(this));
            uint256 minOut = target > cur ? target - cur : 1;
            _swapExactInput(cb.collateralToken, cb.loanToken, seized, minOut, cb.poolFee);
        }

        uint256 bal = IERC20(cb.loanToken).balanceOf(address(this));
        profit = bal > base ? bal - base : 0;
        if (profit < cb.minNetProfit) revert InsufficientProfit(cb.minNetProfit, profit);
    }

    function _swapExactInput(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 amountOutMin,
        uint24 fee
    ) internal returns (uint256 amountOut) {
        address router = swapRouter;
        _safeApprove(tokenIn, router, amountIn); // router pulls exactly amountIn

        amountOut = ISwapRouter(router).exactInputSingle(
            ISwapRouter.ExactInputSingleParams({
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                recipient: address(this),
                amountIn: amountIn,
                amountOutMinimum: amountOutMin,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            })
        );
        if (amountOut == 0) revert SwapFailed();
    }

    // ------------------------------------------------------------------
    // Admin
    // ------------------------------------------------------------------

    function setSwapRouter(address router_) external onlyOwner {
        if (router_ == address(0)) revert ZeroAddress();
        swapRouter = router_;
        emit SwapRouterUpdated(router_);
    }

    function setDefaultPoolFee(uint24 fee) external onlyOwner {
        defaultPoolFee = fee;
        emit PoolFeeUpdated(fee);
    }

    function setSqrtPriceLimitX96(uint160 limit) external onlyOwner {
        sqrtPriceLimitX96 = limit;
        emit PriceLimitUpdated(limit);
    }

    function setOperator(address operator, bool allowed) external onlyOwner {
        if (operator == address(0)) revert ZeroAddress();
        operators[operator] = allowed;
        emit OperatorUpdated(operator, allowed);
    }

    function setPaused(bool paused_) external onlyOwner {
        paused = paused_;
    }

    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner, newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert Unauthorized();
        emit OwnershipTransferred(owner, msg.sender);
        owner = msg.sender;
        pendingOwner = address(0);
    }

    /// @notice Sweep accumulated profit (or any other token) to the owner.
    function rescue(address token, uint256 amount) external onlyOwner {
        _safeTransfer(token, owner, amount);
    }

    // ------------------------------------------------------------------
    // Minimal SafeERC20 (handles tokens that return nothing, e.g. USDT)
    // ------------------------------------------------------------------

    function _callOptionalReturn(address token, bytes memory callData) private returns (bool) {
        (bool success, bytes memory ret) = token.call(callData);
        if (!success) return false;
        if (ret.length == 0) return token.code.length > 0;
        return ret.length >= 32 && abi.decode(ret, (bool));
    }

    function _safeTransfer(address token, address to, uint256 amount) internal {
        if (!_callOptionalReturn(token, abi.encodeCall(IERC20.transfer, (to, amount)))) {
            revert TransferFailed();
        }
    }

    /// @dev Direct approve first; on failure reset to 0 then set (USDT-style).
    function _safeApprove(address token, address spender, uint256 amount) internal {
        if (_callOptionalReturn(token, abi.encodeCall(IERC20.approve, (spender, amount)))) return;
        if (!_callOptionalReturn(token, abi.encodeCall(IERC20.approve, (spender, 0)))) {
            revert TransferFailed();
        }
        if (amount > 0 && !_callOptionalReturn(token, abi.encodeCall(IERC20.approve, (spender, amount)))) {
            revert TransferFailed();
        }
    }
}