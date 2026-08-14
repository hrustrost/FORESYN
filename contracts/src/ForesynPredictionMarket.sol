// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.20;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";

/// @title ForesynPredictionMarket
/// @notice Binary, native-ETH, pari-mutuel settlement for the Foresyn prototype.
/// @dev Descriptive market data remains off-chain and is committed by metadataDigest.
contract ForesynPredictionMarket is Ownable, Pausable, ReentrancyGuard {
    enum Outcome {
        Unset,
        Yes,
        No
    }

    enum MarketStatus {
        Open,
        Resolved,
        Cancelled
    }

    enum CancellationReason {
        ZeroWinningPool,
        ResolverCancellation
    }

    struct Market {
        // These four values share one storage slot.
        address resolver;
        uint64 deadline;
        MarketStatus status;
        Outcome winningOutcome;
        bytes32 metadataDigest;
        uint256 yesPool;
        uint256 noPool;
        uint256 claimedWinningStake;
        uint256 claimedAmount;
    }

    struct Position {
        uint256 yesStake;
        uint256 noStake;
        bool claimed;
    }

    error MarketNotFound(uint256 marketId);
    error DeadlineNotInFuture(uint256 deadline, uint256 currentTime);
    error InvalidResolver();
    error InvalidMetadataDigest();
    error InvalidOutcome(uint8 outcome);
    error ZeroStake();
    error MarketNotOpen(uint256 marketId, MarketStatus currentStatus);
    error BettingClosed(uint256 marketId, uint256 deadline);
    error UnauthorizedResolver(uint256 marketId, address caller);
    error DeadlineNotReached(uint256 marketId, uint256 deadline);
    error MarketNotTerminal(uint256 marketId);
    error AlreadyClaimed(uint256 marketId, address user);
    error NoClaimableStake(uint256 marketId, address user);
    error EthTransferFailed(address recipient, uint256 amount);

    event MarketCreated(
        uint256 indexed marketId, address indexed resolver, address creator, uint64 deadline, bytes32 metadataDigest
    );
    event PositionTaken(
        uint256 indexed marketId,
        address indexed user,
        Outcome outcome,
        uint256 amount,
        uint256 userOutcomeStake,
        uint256 yesPool,
        uint256 noPool
    );
    event MarketResolved(
        uint256 indexed marketId, address indexed resolver, Outcome outcome, uint256 totalPool, uint256 winningPool
    );
    event MarketCancelled(
        uint256 indexed marketId,
        address indexed resolver,
        CancellationReason reason,
        Outcome attemptedOutcome,
        uint256 totalPool
    );
    event WinningsClaimed(
        uint256 indexed marketId, address indexed user, Outcome outcome, uint256 winningStake, uint256 payout
    );
    event RefundClaimed(uint256 indexed marketId, address indexed user, uint256 amount);

    uint256 public nextMarketId = 1;

    mapping(uint256 marketId => Market) private _markets;
    mapping(uint256 marketId => mapping(address user => Position)) private _positions;

    constructor(address initialOwner) Ownable(initialOwner) {}

    /// @notice Creates a market linked to immutable off-chain rules by digest.
    function createMarket(uint64 deadline, address resolver, bytes32 metadataDigest)
        external
        onlyOwner
        whenNotPaused
        returns (uint256 marketId)
    {
        if (deadline <= block.timestamp) {
            revert DeadlineNotInFuture(deadline, block.timestamp);
        }
        if (resolver == address(0)) revert InvalidResolver();
        if (metadataDigest == bytes32(0)) revert InvalidMetadataDigest();

        marketId = nextMarketId;
        nextMarketId = marketId + 1;

        _markets[marketId] = Market({
            resolver: resolver,
            deadline: deadline,
            status: MarketStatus.Open,
            winningOutcome: Outcome.Unset,
            metadataDigest: metadataDigest,
            yesPool: 0,
            noPool: 0,
            claimedWinningStake: 0,
            claimedAmount: 0
        });

        emit MarketCreated(marketId, resolver, msg.sender, deadline, metadataDigest);
    }

    /// @notice Adds the sent ETH to the caller's selected side of an open market.
    function takePosition(uint256 marketId, Outcome outcome) external payable whenNotPaused {
        Market storage market = _getMarket(marketId);
        _requireBinaryOutcome(outcome);

        if (market.status != MarketStatus.Open) {
            revert MarketNotOpen(marketId, market.status);
        }
        if (block.timestamp >= market.deadline) {
            revert BettingClosed(marketId, market.deadline);
        }
        if (msg.value == 0) revert ZeroStake();

        Position storage position = _positions[marketId][msg.sender];
        uint256 userOutcomeStake;

        if (outcome == Outcome.Yes) {
            position.yesStake += msg.value;
            market.yesPool += msg.value;
            userOutcomeStake = position.yesStake;
        } else {
            position.noStake += msg.value;
            market.noPool += msg.value;
            userOutcomeStake = position.noStake;
        }

        emit PositionTaken(marketId, msg.sender, outcome, msg.value, userOutcomeStake, market.yesPool, market.noPool);
    }

    /// @notice Resolves a market, or cancels it when the selected side has no stake.
    function resolveMarket(uint256 marketId, Outcome outcome) external {
        Market storage market = _getMarket(marketId);
        _requireBinaryOutcome(outcome);

        if (market.status != MarketStatus.Open) {
            revert MarketNotOpen(marketId, market.status);
        }
        if (msg.sender != market.resolver) {
            revert UnauthorizedResolver(marketId, msg.sender);
        }
        if (block.timestamp < market.deadline) {
            revert DeadlineNotReached(marketId, market.deadline);
        }

        uint256 winningPool = outcome == Outcome.Yes ? market.yesPool : market.noPool;
        uint256 totalPool = market.yesPool + market.noPool;

        if (winningPool == 0) {
            market.status = MarketStatus.Cancelled;
            emit MarketCancelled(marketId, msg.sender, CancellationReason.ZeroWinningPool, outcome, totalPool);
            return;
        }

        market.status = MarketStatus.Resolved;
        market.winningOutcome = outcome;

        emit MarketResolved(marketId, msg.sender, outcome, totalPool, winningPool);
    }

    /// @notice Refunds an unresolvable market after its deadline.
    /// @dev Uses the assigned resolver rather than giving the contract owner a second authority path.
    function cancelMarket(uint256 marketId) external {
        Market storage market = _getMarket(marketId);

        if (market.status != MarketStatus.Open) {
            revert MarketNotOpen(marketId, market.status);
        }
        if (msg.sender != market.resolver) {
            revert UnauthorizedResolver(marketId, msg.sender);
        }
        if (block.timestamp < market.deadline) {
            revert DeadlineNotReached(marketId, market.deadline);
        }

        market.status = MarketStatus.Cancelled;

        emit MarketCancelled(
            marketId, msg.sender, CancellationReason.ResolverCancellation, Outcome.Unset, market.yesPool + market.noPool
        );
    }

    /// @notice Pulls winnings or a cancellation refund owed to the caller.
    function claim(uint256 marketId) external nonReentrant returns (uint256 amount) {
        Market storage market = _getMarket(marketId);
        Position storage position = _positions[marketId][msg.sender];

        if (market.status == MarketStatus.Open) revert MarketNotTerminal(marketId);
        if (position.claimed) revert AlreadyClaimed(marketId, msg.sender);

        if (market.status == MarketStatus.Cancelled) {
            amount = position.yesStake + position.noStake;
            if (amount == 0) revert NoClaimableStake(marketId, msg.sender);

            position.claimed = true;
            market.claimedAmount += amount;

            emit RefundClaimed(marketId, msg.sender, amount);
        } else {
            uint256 winningStake = market.winningOutcome == Outcome.Yes ? position.yesStake : position.noStake;
            if (winningStake == 0) revert NoClaimableStake(marketId, msg.sender);

            uint256 totalPool = market.yesPool + market.noPool;
            uint256 totalWinningStake = market.winningOutcome == Outcome.Yes ? market.yesPool : market.noPool;
            uint256 claimedWinningStakeAfter = market.claimedWinningStake + winningStake;

            // The last winner receives the remaining wei so no rounding dust is stranded.
            if (claimedWinningStakeAfter == totalWinningStake) {
                amount = totalPool - market.claimedAmount;
            } else {
                amount = Math.mulDiv(winningStake, totalPool, totalWinningStake);
            }

            position.claimed = true;
            market.claimedWinningStake = claimedWinningStakeAfter;
            market.claimedAmount += amount;

            emit WinningsClaimed(marketId, msg.sender, market.winningOutcome, winningStake, amount);
        }

        // Effects are committed before the external call; a failed call reverts them atomically.
        (bool success,) = msg.sender.call{value: amount}("");
        if (!success) revert EthTransferFailed(msg.sender, amount);
    }

    /// @notice Stops only new markets and new positions during an incident.
    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    function getMarket(uint256 marketId) external view returns (Market memory) {
        Market storage market = _getMarket(marketId);
        return market;
    }

    function getPosition(uint256 marketId, address user) external view returns (Position memory) {
        _getMarket(marketId);
        return _positions[marketId][user];
    }

    function _getMarket(uint256 marketId) private view returns (Market storage market) {
        market = _markets[marketId];
        if (market.resolver == address(0)) revert MarketNotFound(marketId);
    }

    function _requireBinaryOutcome(Outcome outcome) private pure {
        if (outcome == Outcome.Unset) revert InvalidOutcome(uint8(outcome));
    }
}
