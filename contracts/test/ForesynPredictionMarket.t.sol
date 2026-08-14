// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.20;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

import {ForesynPredictionMarket} from "../src/ForesynPredictionMarket.sol";
import {ConfigurableReceiver, ReentrantClaimer} from "./helpers/ClaimReceivers.sol";

contract ForesynPredictionMarketTest is Test {
    event MarketCreated(
        uint256 indexed marketId, address indexed resolver, address creator, uint64 deadline, bytes32 metadataDigest
    );
    event PositionTaken(
        uint256 indexed marketId,
        address indexed user,
        ForesynPredictionMarket.Outcome outcome,
        uint256 amount,
        uint256 userOutcomeStake,
        uint256 yesPool,
        uint256 noPool
    );
    event MarketResolved(
        uint256 indexed marketId,
        address indexed resolver,
        ForesynPredictionMarket.Outcome outcome,
        uint256 totalPool,
        uint256 winningPool
    );
    event MarketCancelled(
        uint256 indexed marketId,
        address indexed resolver,
        ForesynPredictionMarket.CancellationReason reason,
        ForesynPredictionMarket.Outcome attemptedOutcome,
        uint256 totalPool
    );
    event WinningsClaimed(
        uint256 indexed marketId,
        address indexed user,
        ForesynPredictionMarket.Outcome outcome,
        uint256 winningStake,
        uint256 payout
    );
    event RefundClaimed(uint256 indexed marketId, address indexed user, uint256 amount);

    ForesynPredictionMarket internal market;

    address internal resolver;
    address internal alice;
    address internal bob;
    address internal carol;
    address internal stranger;

    uint64 internal deadline;
    bytes32 internal constant METADATA_DIGEST = keccak256("foresyn-market-rules-v1");

    function setUp() public {
        vm.warp(1_700_000_000);

        resolver = makeAddr("resolver");
        alice = makeAddr("alice");
        bob = makeAddr("bob");
        carol = makeAddr("carol");
        stranger = makeAddr("stranger");
        deadline = uint64(block.timestamp + 7 days);

        market = new ForesynPredictionMarket(address(this));

        vm.deal(alice, 1_000 ether);
        vm.deal(bob, 1_000 ether);
        vm.deal(carol, 1_000 ether);
        vm.deal(stranger, 1_000 ether);
    }

    function test_createMarketStoresMinimalSettlementStateAndEmitsEvent() public {
        vm.expectEmit(true, true, false, true, address(market));
        emit MarketCreated(1, resolver, address(this), deadline, METADATA_DIGEST);

        uint256 marketId = _createMarket();
        ForesynPredictionMarket.Market memory created = market.getMarket(marketId);

        assertEq(marketId, 1);
        assertEq(market.nextMarketId(), 2);
        assertEq(created.resolver, resolver);
        assertEq(created.deadline, deadline);
        assertEq(uint8(created.status), uint8(ForesynPredictionMarket.MarketStatus.Open));
        assertEq(uint8(created.winningOutcome), uint8(ForesynPredictionMarket.Outcome.Unset));
        assertEq(created.metadataDigest, METADATA_DIGEST);
        assertEq(created.yesPool, 0);
        assertEq(created.noPool, 0);
        assertEq(created.claimedWinningStake, 0);
        assertEq(created.claimedAmount, 0);
    }

    function test_marketIdsAreSequential() public {
        assertEq(_createMarket(), 1);
        assertEq(market.createMarket(deadline + 1, resolver, keccak256("second")), 2);
        assertEq(market.nextMarketId(), 3);
    }

    function test_createMarketRejectsPastDeadline() public {
        uint64 pastDeadline = uint64(block.timestamp - 1);
        vm.expectRevert(
            abi.encodeWithSelector(ForesynPredictionMarket.DeadlineNotInFuture.selector, pastDeadline, block.timestamp)
        );
        market.createMarket(pastDeadline, resolver, METADATA_DIGEST);
    }

    function test_createMarketRejectsCurrentTimestampDeadline() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                ForesynPredictionMarket.DeadlineNotInFuture.selector, block.timestamp, block.timestamp
            )
        );
        market.createMarket(uint64(block.timestamp), resolver, METADATA_DIGEST);
    }

    function test_createMarketRejectsZeroResolver() public {
        vm.expectRevert(ForesynPredictionMarket.InvalidResolver.selector);
        market.createMarket(deadline, address(0), METADATA_DIGEST);
    }

    function test_createMarketRejectsZeroMetadataDigest() public {
        vm.expectRevert(ForesynPredictionMarket.InvalidMetadataDigest.selector);
        market.createMarket(deadline, resolver, bytes32(0));
    }

    function test_nonOwnerCannotCreateMarket() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger));
        market.createMarket(deadline, resolver, METADATA_DIGEST);
    }

    function test_takeYesPositionUpdatesStakeAndPool() public {
        uint256 marketId = _createMarket();

        vm.expectEmit(true, true, false, true, address(market));
        emit PositionTaken(marketId, alice, ForesynPredictionMarket.Outcome.Yes, 2 ether, 2 ether, 2 ether, 0);
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 2 ether);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        ForesynPredictionMarket.Position memory position = market.getPosition(marketId, alice);
        assertEq(current.yesPool, 2 ether);
        assertEq(current.noPool, 0);
        assertEq(position.yesStake, 2 ether);
        assertEq(position.noStake, 0);
    }

    function test_takeNoPositionUpdatesStakeAndPool() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, 3 ether);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        ForesynPredictionMarket.Position memory position = market.getPosition(marketId, alice);
        assertEq(current.yesPool, 0);
        assertEq(current.noPool, 3 ether);
        assertEq(position.yesStake, 0);
        assertEq(position.noStake, 3 ether);
    }

    function test_walletMayAddToExistingStakeAndHoldBothOutcomes() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 1 ether);
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 2 ether);
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, 4 ether);

        ForesynPredictionMarket.Position memory position = market.getPosition(marketId, alice);
        assertEq(position.yesStake, 3 ether);
        assertEq(position.noStake, 4 ether);
    }

    function test_takePositionRejectsZeroEth() public {
        uint256 marketId = _createMarket();
        vm.prank(alice);
        vm.expectRevert(ForesynPredictionMarket.ZeroStake.selector);
        market.takePosition(marketId, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_takePositionRejectsInvalidMarket() public {
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.MarketNotFound.selector, 999));
        market.takePosition{value: 1 ether}(999, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_takePositionRejectsUnsetOutcome() public {
        uint256 marketId = _createMarket();
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.InvalidOutcome.selector, 0));
        market.takePosition{value: 1 ether}(marketId, ForesynPredictionMarket.Outcome.Unset);
    }

    function testFuzz_positionAtOrAfterDeadlineIsRejected(uint32 secondsAfterDeadline) public {
        uint256 marketId = _createMarket();
        uint256 offset = bound(secondsAfterDeadline, 0, 365 days);
        vm.warp(uint256(deadline) + offset);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.BettingClosed.selector, marketId, deadline));
        market.takePosition{value: 1 ether}(marketId, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_unauthorizedResolverIsRejected() public {
        uint256 marketId = _seedBalancedMarket();
        vm.warp(deadline);

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(ForesynPredictionMarket.UnauthorizedResolver.selector, marketId, stranger)
        );
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_resolutionBeforeDeadlineIsRejected() public {
        uint256 marketId = _seedBalancedMarket();

        vm.prank(resolver);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.DeadlineNotReached.selector, marketId, deadline));
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_resolvesYesAtDeadline() public {
        uint256 marketId = _seedBalancedMarket();
        vm.warp(deadline);

        vm.expectEmit(true, true, false, true, address(market));
        emit MarketResolved(marketId, resolver, ForesynPredictionMarket.Outcome.Yes, 3 ether, 2 ether);
        vm.prank(resolver);
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.Yes);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        assertEq(uint8(current.status), uint8(ForesynPredictionMarket.MarketStatus.Resolved));
        assertEq(uint8(current.winningOutcome), uint8(ForesynPredictionMarket.Outcome.Yes));
    }

    function test_resolvesNoAtDeadline() public {
        uint256 marketId = _seedBalancedMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.No);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        assertEq(uint8(current.status), uint8(ForesynPredictionMarket.MarketStatus.Resolved));
        assertEq(uint8(current.winningOutcome), uint8(ForesynPredictionMarket.Outcome.No));
    }

    function test_doubleResolutionIsRejected() public {
        uint256 marketId = _seedBalancedMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        vm.prank(resolver);
        vm.expectRevert(
            abi.encodeWithSelector(
                ForesynPredictionMarket.MarketNotOpen.selector, marketId, ForesynPredictionMarket.MarketStatus.Resolved
            )
        );
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.No);
    }

    function test_resolveMarketRejectsUnsetOutcome() public {
        uint256 marketId = _seedBalancedMarket();
        vm.warp(deadline);

        vm.prank(resolver);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.InvalidOutcome.selector, 0));
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.Unset);
    }

    function test_resolverCanCancelOnlyAfterDeadline() public {
        uint256 marketId = _seedBalancedMarket();

        vm.prank(resolver);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.DeadlineNotReached.selector, marketId, deadline));
        market.cancelMarket(marketId);
    }

    function test_unauthorizedCancellationIsRejected() public {
        uint256 marketId = _seedBalancedMarket();
        vm.warp(deadline);

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(ForesynPredictionMarket.UnauthorizedResolver.selector, marketId, stranger)
        );
        market.cancelMarket(marketId);
    }

    function test_winnerReceivesCompletePoolWhenOnlyWinner() public {
        uint256 marketId = _seedBalancedMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        vm.expectEmit(true, true, false, true, address(market));
        emit WinningsClaimed(marketId, alice, ForesynPredictionMarket.Outcome.Yes, 2 ether, 3 ether);
        assertEq(_claimPayout(alice, marketId), 3 ether);
        assertEq(address(market).balance, 0);
    }

    function test_losingUserCannotClaimWinningPayout() public {
        uint256 marketId = _seedBalancedMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.NoClaimableStake.selector, marketId, bob));
        market.claim(marketId);
    }

    function test_claimBeforeResolutionIsRejected() public {
        uint256 marketId = _seedBalancedMarket();

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.MarketNotTerminal.selector, marketId));
        market.claim(marketId);
    }

    function test_doubleClaimIsRejected() public {
        uint256 marketId = _seedBalancedMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);
        _claimPayout(alice, marketId);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.AlreadyClaimed.selector, marketId, alice));
        market.claim(marketId);
    }

    function test_multipleWinnersReceiveProportionalPayouts() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 1 ether);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.Yes, 3 ether);
        _take(carol, marketId, ForesynPredictionMarket.Outcome.No, 4 ether);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        assertEq(_claimPayout(alice, marketId), 2 ether);
        assertEq(_claimPayout(bob, marketId), 6 ether);
        assertEq(address(market).balance, 0);
    }

    function test_integerRoundingUsesFloorForNonFinalWinner() public {
        uint256 marketId = _seedRoundingMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        assertEq(_claimPayout(alice, marketId), 1 wei);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        assertEq(current.claimedAmount, 1 wei);
        assertEq(address(market).balance, 3 wei);
    }

    function test_finalWinnerReceivesRoundingRemainder() public {
        uint256 marketId = _seedRoundingMarket();
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        assertEq(_claimPayout(alice, marketId), 1 wei);
        assertEq(_claimPayout(bob, marketId), 3 wei);

        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        assertEq(current.claimedAmount, 4 wei);
        assertEq(current.claimedWinningStake, 3 wei);
        assertEq(address(market).balance, 0);
    }

    function test_zeroWinningPoolCancelsAndRefundsNonEmptySide() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, 5 ether);
        vm.warp(deadline);

        vm.expectEmit(true, true, false, true, address(market));
        emit MarketCancelled(
            marketId,
            resolver,
            ForesynPredictionMarket.CancellationReason.ZeroWinningPool,
            ForesynPredictionMarket.Outcome.Yes,
            5 ether
        );
        vm.prank(resolver);
        market.resolveMarket(marketId, ForesynPredictionMarket.Outcome.Yes);

        assertEq(_claimPayout(alice, marketId), 5 ether);
        assertEq(address(market).balance, 0);
    }

    function test_resolverCancellationRefundsBothYesAndNoStake() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 2 ether);
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, 3 ether);
        vm.warp(deadline);

        vm.expectEmit(true, true, false, true, address(market));
        emit MarketCancelled(
            marketId,
            resolver,
            ForesynPredictionMarket.CancellationReason.ResolverCancellation,
            ForesynPredictionMarket.Outcome.Unset,
            5 ether
        );
        vm.prank(resolver);
        market.cancelMarket(marketId);

        vm.expectEmit(true, true, false, true, address(market));
        emit RefundClaimed(marketId, alice, 5 ether);
        assertEq(_claimPayout(alice, marketId), 5 ether);
    }

    function test_doubleRefundIsRejected() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, 1 ether);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);
        _claimPayout(alice, marketId);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.AlreadyClaimed.selector, marketId, alice));
        market.claim(marketId);
    }

    function test_nonParticipantCannotClaimCancelledMarket() public {
        uint256 marketId = _createMarket();
        vm.warp(deadline);
        vm.prank(resolver);
        market.cancelMarket(marketId);

        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ForesynPredictionMarket.NoClaimableStake.selector, marketId, stranger));
        market.claim(marketId);
    }

    function test_pauseBlocksMarketCreation() public {
        market.pause();
        vm.expectRevert(Pausable.EnforcedPause.selector);
        market.createMarket(deadline, resolver, METADATA_DIGEST);
    }

    function test_pauseBlocksNewPositions() public {
        uint256 marketId = _createMarket();
        market.pause();

        vm.prank(alice);
        vm.expectRevert(Pausable.EnforcedPause.selector);
        market.takePosition{value: 1 ether}(marketId, ForesynPredictionMarket.Outcome.Yes);
    }

    function test_unauthorizedUserCannotPauseOrUnpause() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger));
        market.pause();

        market.pause();
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger));
        market.unpause();
    }

    function test_unpauseRestoresCreationAndPositions() public {
        market.pause();
        market.unpause();

        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 1 ether);

        assertEq(market.getPosition(marketId, alice).yesStake, 1 ether);
    }

    function test_pauseDoesNotFreezeResolutionOrClaims() public {
        uint256 marketId = _seedBalancedMarket();
        market.pause();

        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);
        assertEq(_claimPayout(alice, marketId), 3 ether);
    }

    function test_reentrantClaimCannotWithdrawTwice() public {
        uint256 marketId = _createMarket();
        ReentrantClaimer attacker = new ReentrantClaimer(market);
        attacker.takePosition{value: 1 ether}(marketId, ForesynPredictionMarket.Outcome.Yes);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.No, 1 ether);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        attacker.attackClaim(marketId);

        assertTrue(attacker.attemptedReentry());
        assertFalse(attacker.reentrySucceeded());
        assertEq(address(attacker).balance, 2 ether);
        assertTrue(market.getPosition(marketId, address(attacker)).claimed);
        assertEq(market.getMarket(marketId).claimedAmount, 2 ether);
    }

    function test_failedEthReceiverRevertsStateAndCanRetry() public {
        uint256 marketId = _createMarket();
        ConfigurableReceiver receiver = new ConfigurableReceiver(market);
        receiver.takePosition{value: 1 ether}(marketId, ForesynPredictionMarket.Outcome.Yes);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.No, 1 ether);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        vm.expectRevert(
            abi.encodeWithSelector(ForesynPredictionMarket.EthTransferFailed.selector, address(receiver), 2 ether)
        );
        receiver.claim(marketId);

        assertFalse(market.getPosition(marketId, address(receiver)).claimed);
        assertEq(market.getMarket(marketId).claimedAmount, 0);

        receiver.setRejectEth(false);
        receiver.claim(marketId);
        assertEq(address(receiver).balance, 2 ether);
        assertTrue(market.getPosition(marketId, address(receiver)).claimed);
    }

    function test_totalPayoutNeverExceedsDepositedPool() public {
        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 1 ether);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.Yes, 2 ether);
        _take(carol, marketId, ForesynPredictionMarket.Outcome.No, 5 ether);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        uint256 totalDeposited = 8 ether;
        _claimPayout(alice, marketId);
        assertLe(market.getMarket(marketId).claimedAmount, totalDeposited);
        _claimPayout(bob, marketId);
        assertEq(market.getMarket(marketId).claimedAmount, totalDeposited);
        assertEq(address(market).balance, 0);
    }

    function testFuzz_proportionalPayoutsRemainSolvent(
        uint128 aliceStakeSeed,
        uint128 bobStakeSeed,
        uint128 losingStakeSeed,
        bool aliceClaimsFirst
    ) public {
        uint256 aliceStake = bound(aliceStakeSeed, 1, type(uint128).max);
        uint256 bobStake = bound(bobStakeSeed, 1, type(uint128).max);
        uint256 losingStake = bound(losingStakeSeed, 1, type(uint128).max);
        uint256 totalDeposited = aliceStake + bobStake + losingStake;

        vm.deal(alice, aliceStake);
        vm.deal(bob, bobStake);
        vm.deal(carol, losingStake);

        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, aliceStake);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.Yes, bobStake);
        _take(carol, marketId, ForesynPredictionMarket.Outcome.No, losingStake);
        _resolve(marketId, ForesynPredictionMarket.Outcome.Yes);

        uint256 firstPayout;
        uint256 secondPayout;
        if (aliceClaimsFirst) {
            firstPayout = _claimPayout(alice, marketId);
            secondPayout = _claimPayout(bob, marketId);
        } else {
            firstPayout = _claimPayout(bob, marketId);
            secondPayout = _claimPayout(alice, marketId);
        }

        assertLe(firstPayout, totalDeposited);
        assertEq(firstPayout + secondPayout, totalDeposited);
        assertEq(market.getMarket(marketId).claimedAmount, totalDeposited);
        assertEq(address(market).balance, 0);
    }

    function testFuzz_cancelledMarketRefundsBothSides(uint128 yesSeed, uint128 noSeed) public {
        uint256 yesStake = bound(yesSeed, 1, type(uint128).max);
        uint256 noStake = bound(noSeed, 1, type(uint128).max);
        vm.deal(alice, yesStake + noStake);

        uint256 marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, yesStake);
        _take(alice, marketId, ForesynPredictionMarket.Outcome.No, noStake);

        vm.warp(deadline);
        vm.prank(resolver);
        market.cancelMarket(marketId);

        assertEq(_claimPayout(alice, marketId), yesStake + noStake);
        assertEq(market.getMarket(marketId).claimedAmount, yesStake + noStake);
        assertEq(address(market).balance, 0);
    }

    function _createMarket() internal returns (uint256) {
        return market.createMarket(deadline, resolver, METADATA_DIGEST);
    }

    function _seedBalancedMarket() internal returns (uint256 marketId) {
        marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 2 ether);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.No, 1 ether);
    }

    function _seedRoundingMarket() internal returns (uint256 marketId) {
        marketId = _createMarket();
        _take(alice, marketId, ForesynPredictionMarket.Outcome.Yes, 1 wei);
        _take(bob, marketId, ForesynPredictionMarket.Outcome.Yes, 2 wei);
        _take(carol, marketId, ForesynPredictionMarket.Outcome.No, 1 wei);
    }

    function _take(address user, uint256 marketId, ForesynPredictionMarket.Outcome outcome, uint256 amount) internal {
        vm.prank(user);
        market.takePosition{value: amount}(marketId, outcome);
    }

    function _resolve(uint256 marketId, ForesynPredictionMarket.Outcome outcome) internal {
        vm.warp(deadline);
        vm.prank(resolver);
        market.resolveMarket(marketId, outcome);
    }

    function _claimPayout(address user, uint256 marketId) internal returns (uint256 payout) {
        uint256 balanceBefore = user.balance;
        vm.prank(user);
        market.claim(marketId);
        payout = user.balance - balanceBefore;
    }
}
