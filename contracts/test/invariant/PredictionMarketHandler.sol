// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.20;

import {Test} from "forge-std/Test.sol";

import {ForesynPredictionMarket} from "../../src/ForesynPredictionMarket.sol";

contract PredictionMarketHandler is Test {
    ForesynPredictionMarket public immutable market;
    uint256 public immutable marketId;
    address public immutable resolver;
    uint64 public immutable deadline;

    uint256 public ghostDeposited;
    uint256 public ghostPaid;

    mapping(address actor => uint256 count) public successfulClaims;

    address[] private _actors;

    constructor(ForesynPredictionMarket market_, uint256 marketId_, address resolver_, uint64 deadline_) {
        market = market_;
        marketId = marketId_;
        resolver = resolver_;
        deadline = deadline_;

        _actors.push(address(0xA11CE));
        _actors.push(address(0xB0B));
        _actors.push(address(0xCA401));
        _actors.push(address(0xD00D));
    }

    function takePosition(uint256 actorSeed, uint96 amountSeed, bool takeYes) external {
        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        if (current.status != ForesynPredictionMarket.MarketStatus.Open || block.timestamp >= deadline) return;

        address actor = actorAt(actorSeed);
        uint256 amount = bound(amountSeed, 1, 10 ether);
        ForesynPredictionMarket.Outcome outcome =
            takeYes ? ForesynPredictionMarket.Outcome.Yes : ForesynPredictionMarket.Outcome.No;

        vm.deal(actor, actor.balance + amount);
        vm.prank(actor);
        market.takePosition{value: amount}(marketId, outcome);
        ghostDeposited += amount;
    }

    function resolve(bool resolveYes) external {
        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        if (current.status != ForesynPredictionMarket.MarketStatus.Open) return;

        vm.warp(deadline);
        vm.prank(resolver);
        market.resolveMarket(
            marketId, resolveYes ? ForesynPredictionMarket.Outcome.Yes : ForesynPredictionMarket.Outcome.No
        );
    }

    function cancel() external {
        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        if (current.status != ForesynPredictionMarket.MarketStatus.Open) return;

        vm.warp(deadline);
        vm.prank(resolver);
        market.cancelMarket(marketId);
    }

    function claim(uint256 actorSeed) external {
        ForesynPredictionMarket.Market memory current = market.getMarket(marketId);
        if (current.status == ForesynPredictionMarket.MarketStatus.Open) return;

        address actor = actorAt(actorSeed);
        uint256 balanceBefore = actor.balance;

        vm.prank(actor);
        try market.claim(marketId) {
            ghostPaid += actor.balance - balanceBefore;
            successfulClaims[actor] += 1;
        } catch {}
    }

    function actorAt(uint256 seed) public view returns (address) {
        return _actors[seed % _actors.length];
    }

    function actorCount() external view returns (uint256) {
        return _actors.length;
    }
}

