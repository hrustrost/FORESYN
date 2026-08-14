// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.20;

import {StdInvariant} from "forge-std/StdInvariant.sol";
import {Test} from "forge-std/Test.sol";

import {ForesynPredictionMarket} from "../../src/ForesynPredictionMarket.sol";
import {PredictionMarketHandler} from "./PredictionMarketHandler.sol";

contract ForesynPredictionMarketInvariantTest is StdInvariant, Test {
    ForesynPredictionMarket internal market;
    PredictionMarketHandler internal handler;

    uint256 internal marketId;

    function setUp() public {
        vm.warp(1_700_000_000);

        address resolver = makeAddr("invariant-resolver");
        uint64 deadline = uint64(block.timestamp + 30 days);

        market = new ForesynPredictionMarket(address(this));
        marketId = market.createMarket(deadline, resolver, keccak256("stateful-invariant-market"));
        handler = new PredictionMarketHandler(market, marketId, resolver, deadline);

        bytes4[] memory selectors = new bytes4[](4);
        selectors[0] = PredictionMarketHandler.takePosition.selector;
        selectors[1] = PredictionMarketHandler.resolve.selector;
        selectors[2] = PredictionMarketHandler.cancel.selector;
        selectors[3] = PredictionMarketHandler.claim.selector;

        targetContract(address(handler));
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
    }

    function invariant_cumulativeSettlementNeverExceedsDeposits() public view {
        assertLe(handler.ghostPaid(), handler.ghostDeposited());
    }

    function invariant_depositsAreEitherHeldOrPaid() public view {
        assertEq(address(market).balance + handler.ghostPaid(), handler.ghostDeposited());
    }

    function invariant_marketAccountingNeverExceedsItsPool() public view {
        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        uint256 totalPool = current.yesPool + current.noPool;

        assertLe(current.claimedAmount, totalPool);
        if (current.status == ForesynPredictionMarket.MarketStatus.Resolved) {
            uint256 winningPool =
                current.winningOutcome == ForesynPredictionMarket.Outcome.Yes ? current.yesPool : current.noPool;
            assertLe(current.claimedWinningStake, winningPool);
        } else {
            assertEq(current.claimedWinningStake, 0);
        }
    }

    function invariant_eachPositionCanSettleAtMostOnce() public view {
        uint256 actors = handler.actorCount();
        for (uint256 i = 0; i < actors; i++) {
            assertLe(handler.successfulClaims(handler.actorAt(i)), 1);
        }
    }
}
