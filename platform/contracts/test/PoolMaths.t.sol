// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Settlement} from "../src/Settlement.sol";
import {Base} from "./helpers/Base.sol";

/// Fuzzed pool maths: whatever the stakes and the outcome, payouts plus the
/// fee never exceed the pool, and the dust left over is at most a unit per
/// claimant.
contract PoolMathsTest is Base {
    function test_fuzz_payouts_and_fee_never_exceed_the_pool(
        uint8 entrantsSeed,
        uint256[8] memory amounts,
        uint8[8] memory picks,
        uint8 outcomeSeed,
        uint8 tieSeed
    ) public {
        uint256 n = 2 + entrantsSeed % 5; // 2 to 6 entrants
        Session memory s = newSession(n);
        address[8] memory who;
        for (uint256 i = 0; i < 8; i++) {
            who[i] = bettor(string(abi.encodePacked("b", vm.toString(i))));
            uint256 amount = bound(amounts[i], USDC, 1_000_000 * USDC);
            bet(who[i], s, picks[i] % n, amount);
        }
        closeAndAnchor(s);
        uint8[] memory places = new uint8[](n);
        uint256 winner = outcomeSeed % n;
        for (uint256 i = 0; i < n; i++) {
            places[i] = uint8(i == winner || (tieSeed % 3 == 0 && i == (winner + 1) % n) ? 0 : 1 + i);
        }
        if (outcomeSeed % 7 == 0) resolve(s, voidOutcome(1));
        else resolve(s, order(places));

        (,,,, uint128 total) = settlement.pools(s.id);
        (uint256 feeTotal,,) = settlement.fee(s.id);
        uint256 paid;
        for (uint256 i = 0; i < 8; i++) {
            paid += claimOne(who[i], s);
        }
        Settlement.FeeItem[] memory items = new Settlement.FeeItem[](1);
        items[0] = Settlement.FeeItem(s.id, s.spec.payouts);
        uint256 before = usdc.balanceOf(address(settlement));
        settlement.claimFees(items);
        uint256 feePaid = before - usdc.balanceOf(address(settlement));
        assertEq(feePaid, feeTotal);
        assertLe(paid + feePaid, total, "never more than the pool");
        assertGe(paid + feePaid + 8, total, "at most a unit of dust per claimant");
        assertEq(usdc.balanceOf(address(settlement)), total - paid - feePaid);
    }
}
