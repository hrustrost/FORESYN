// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.20;

import {ForesynPredictionMarket} from "../../src/ForesynPredictionMarket.sol";

contract ReentrantClaimer {
    ForesynPredictionMarket public immutable market;

    uint256 public marketId;
    bool public attemptedReentry;
    bool public reentrySucceeded;

    constructor(ForesynPredictionMarket market_) {
        market = market_;
    }

    function takePosition(uint256 marketId_, ForesynPredictionMarket.Outcome outcome) external payable {
        market.takePosition{value: msg.value}(marketId_, outcome);
    }

    function attackClaim(uint256 marketId_) external {
        marketId = marketId_;
        market.claim(marketId_);
    }

    receive() external payable {
        if (!attemptedReentry) {
            attemptedReentry = true;
            (reentrySucceeded,) = address(market).call(abi.encodeCall(ForesynPredictionMarket.claim, (marketId)));
        }
    }
}

contract ConfigurableReceiver {
    ForesynPredictionMarket public immutable market;

    bool public rejectEth = true;

    constructor(ForesynPredictionMarket market_) {
        market = market_;
    }

    function setRejectEth(bool rejectEth_) external {
        rejectEth = rejectEth_;
    }

    function takePosition(uint256 marketId, ForesynPredictionMarket.Outcome outcome) external payable {
        market.takePosition{value: msg.value}(marketId, outcome);
    }

    function claim(uint256 marketId) external {
        market.claim(marketId);
    }

    receive() external payable {
        if (rejectEth) revert("ETH rejected");
    }
}
