// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";
import {Base} from "./helpers/Base.sol";

/// The week (COMPETITION.md#the-week): the final's binding at its anchor
/// (TRUST.md#the-finals-gate), calling off a week (TRUST.md#admin-powers), and
/// the week-winner pool (MONEY.md#the-week-winner-pool) in every settlement
/// case.
contract WeeksTest is Base {
    address alice;
    address bob;
    address carol;

    function setUp() public override {
        super.setUp();
        alice = bettor("alice");
        bob = bettor("bob");
        carol = bettor("carol");
    }

    function weeksList(bytes32 id) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = id;
    }

    function anchored(bytes32 id) internal view returns (bool) {
        (address key,,,) = results.anchors(id);
        return key != address(0);
    }

    // ------------------------------------------------------- the binding

    function test_a_final_binds_its_week_at_its_anchor() public {
        Week memory w = newWeek(4);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        vm.warp(f.close + results.ANCHOR_MARGIN());
        vm.expectEmit(true, true, false, false);
        emit Results.WeekBound(w.id, f.id);
        anchorFinal(w, f);
        assertTrue(anchored(f.id));
        (bool cancelled, bytes32 bound) = results.weekState(w.id);
        assertFalse(cancelled);
        assertEq(bound, f.id);
    }

    function test_a_league_session_anchors_without_a_field_entry_and_binds_nothing() public {
        Week memory w = newWeek(4);
        Session memory s = weekSession(w, 2, uint64(vm.getBlockTimestamp() + 6 hours), 1);
        closeAndAnchor(s);
        assertTrue(anchored(s.id));
        (, bytes32 bound) = results.weekState(w.id);
        assertEq(bound, bytes32(0));
    }

    function test_a_second_final_for_a_week_is_refused() public {
        Week memory w = newWeek(4);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        Session memory f2 = weekSession(w, 3, w.finalClose, 0);
        anchorFinal(w, f);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f2.id, "week already has a final");
        anchorFinal(w, f2);
        assertFalse(anchored(f2.id));
    }

    function test_a_final_for_a_called_off_week_is_refused() public {
        Week memory w = newWeek(4);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        vm.prank(admin);
        results.cancelWeeks(weeksList(w.id));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "week called off");
        anchorFinal(w, f);
        assertFalse(anchored(f.id));
    }

    function test_a_final_whose_close_isn_t_the_committed_one_is_refused() public {
        Week memory w = newWeek(4);
        Session memory f = weekSession(w, 2, w.finalClose - 1, 0);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "close isn't the committed final close");
        anchorFinal(w, f);
        assertFalse(anchored(f.id));
    }

    function test_a_final_carrying_anything_but_its_week_s_one_field_entry_is_refused() public {
        Week memory w = newWeek(4);
        Week memory other = newWeek(4, keccak256("other"));
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        vm.warp(f.close + results.ANCHOR_MARGIN());
        vm.roll(block.number + 1);
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = finalItem(other, f);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "not the week's field entry");
        anchorBatch(enclave, items);
        items[0] = finalItem(w, f);
        items[0].fieldEntry = fieldEntry(w.id, keccak256("forged root"), w.poolClose, w.finalClose);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "not the week's field entry");
        anchorBatch(enclave, items);
        items[0] = anchorItem(f);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "not the week's field entry");
        anchorBatch(enclave, items);
        assertFalse(anchored(f.id));
        // With its own field it binds.
        items[0] = finalItem(w, f);
        anchorBatch(enclave, items);
        assertTrue(anchored(f.id));
    }

    function test_a_malformed_field_entry_is_never_a_week_s_field() public {
        bytes32 week = keccak256("a week posted malformed");
        bytes memory bad = fieldEntry(
            week, keccak256("root"), uint64(block.timestamp + 1 days), uint64(block.timestamp + 2 days)
        );
        // A championship word that isn't a bool: decoding it would revert.
        assembly {
            mstore(add(bad, 352), 2)
        }
        assertFalse(results.isFieldEntry(bad));
        bytes[] memory entries = new bytes[](1);
        entries[0] = bad;
        vm.prank(operator);
        results.post(entries);
        (bytes32 hash,) = results.weekFields(week);
        assertEq(hash, bytes32(0), "not recorded, so it can never be decoded");
    }

    function test_a_second_field_entry_for_a_week_is_ignored_everywhere() public {
        Week memory w = newWeek(4);
        // The operator posts another field for the same week: its own final
        // close and a field of its choosing.
        bytes memory second = fieldEntry(w.id, keccak256("rigged"), w.poolClose, w.finalClose + 1 days);
        bytes[] memory entries = new bytes[](1);
        entries[0] = second;
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(w.id, "the week already has a field entry");
        vm.prank(operator);
        results.post(entries);
        (bytes32 hash, uint64 post) = results.weekFields(w.id);
        assertEq(hash, keccak256(w.entry));
        assertEq(post, w.post);
        // No pool opens from it...
        vm.expectRevert(Settlement.NotPosted.selector);
        settlement.openWeekPool(Settlement.WeekOpening({week: w.id, entry: second}));
        // ...and no final binds through it.
        Session memory f = weekSession(w, 2, w.finalClose + 1 days, 0);
        vm.warp(f.close + results.ANCHOR_MARGIN());
        vm.roll(block.number + 1);
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = anchorItem(f);
        items[0].fieldEntry = second;
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "not the week's field entry");
        anchorBatch(enclave, items);
        assertFalse(anchored(f.id));
    }

    function test_a_final_posted_before_the_week_winner_pool_closed_is_refused() public {
        Week memory w = newWeek(4);
        Session memory f = weekSessionNow(w, 2, w.finalClose, 0);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "posted before the week-winner pool closed");
        anchorFinal(w, f);
        assertFalse(anchored(f.id));
    }

    function test_a_week_with_no_championship_has_no_pool_and_no_final() public {
        Week memory w = newWeek(3, keccak256("small"), false);
        Settlement.FieldLeaf memory leaf = w.leaves[0];
        bytes32[] memory proof = leafProof(w, 0);
        Settlement.WeekOpening memory o = weekOpening(w);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "no championship"));
        settlement.openAndBetWeekFor(o, leaf, proof, USDC, alice);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(f.id, "the week has no championship");
        anchorFinal(w, f);
    }

    // ------------------------------------------------------- calling off

    function test_calling_off_a_week_is_the_admin_s_alone_and_stops_once_a_final_is_bound() public {
        Week memory w = newWeek(4);
        Week memory w2 = newWeek(4, keccak256("w2"));
        vm.prank(alice);
        vm.expectRevert(Results.NotAdmin.selector);
        results.cancelWeeks(weeksList(w.id));
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        anchorFinal(w, f);
        bytes32[] memory list = new bytes32[](2);
        list[0] = w.id;
        list[1] = w2.id;
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(w.id, "final already bound");
        vm.prank(admin);
        results.cancelWeeks(list);
        assertFalse(results.weekCancelled(w.id));
        assertTrue(results.weekCancelled(w2.id));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(w2.id, "already called off");
        vm.prank(admin);
        results.cancelWeeks(weeksList(w2.id));
    }

    // ------------------------------------------------------- the pool

    function test_the_first_bet_opens_the_week_pool_from_its_field_entry() public {
        Week memory w = newWeek(6);
        betWeek(alice, w, 3, 5 * USDC);
        (uint40 poolClose, uint40 finalClose, uint32 sides,, uint128 total, bytes32 root) =
            settlement.weekPools(w.id);
        assertEq(poolClose, w.poolClose);
        assertEq(finalClose, w.finalClose);
        assertEq(sides, 1);
        assertEq(total, 5 * USDC);
        assertEq(root, w.root);
        assertEq(settlement.weekBotStake(w.id, w.leaves[3].digest), 5 * USDC);
        betWeek(bob, w, 3, USDC);
        betWeek(bob, w, 1, USDC);
        (,, sides,, total,) = settlement.weekPools(w.id);
        assertEq(sides, 2);
        assertEq(total, 7 * USDC);
    }

    function test_a_bet_on_a_bot_not_in_the_field_or_with_a_bad_proof_is_refused() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, USDC);
        Settlement.FieldLeaf memory leaf = w.leaves[1];
        leaf.payout = alice; // a different payout isn't the leaf the field froze
        bytes32[] memory proof = leafProof(w, 1);
        vm.prank(bob);
        vm.expectRevert(Settlement.NotAnEntrant.selector);
        settlement.betWeekFor(w.id, leaf, proof, USDC, bob);
        vm.prank(bob);
        vm.expectRevert(Settlement.NotAnEntrant.selector);
        settlement.betWeekFor(w.id, w.leaves[2], proof, USDC, bob);
    }

    function test_the_week_pool_closes_at_its_committed_close_and_on_a_called_off_week() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, USDC);
        vm.warp(w.poolClose - 1);
        betWeek(bob, w, 1, USDC);
        vm.warp(w.poolClose);
        Settlement.FieldLeaf memory leaf = w.leaves[1];
        bytes32[] memory proof = leafProof(w, 1);
        vm.prank(bob);
        vm.expectRevert(Settlement.SessionClosed.selector);
        settlement.betWeekFor(w.id, leaf, proof, USDC, bob);

        Week memory w2 = newWeek(4, keccak256("w2"));
        betWeek(alice, w2, 0, USDC);
        vm.prank(admin);
        results.cancelWeeks(weeksList(w2.id));
        leaf = w2.leaves[1];
        proof = leafProof(w2, 1);
        vm.prank(bob);
        vm.expectRevert(Settlement.WeekCalledOff.selector);
        settlement.betWeekFor(w2.id, leaf, proof, USDC, bob);
    }

    function test_a_week_pool_opens_only_from_a_posted_field_entry_within_the_window() public {
        Week memory w = newWeek(4);
        Settlement.WeekOpening memory o = weekOpening(w);
        o.entry = fieldEntry(w.id, w.root, w.poolClose + 1, w.finalClose);
        vm.expectRevert(Settlement.NotPosted.selector);
        settlement.openWeekPool(o);
        // A close less than an hour after its post.
        bytes memory entry =
            fieldEntry(keccak256("soon"), w.root, uint64(vm.getBlockTimestamp() + 30 minutes), w.finalClose);
        bytes[] memory entries = new bytes[](2);
        entries[0] = entry;
        entries[1] = abi.encode(uint8(5), uint32(1));
        vm.prank(operator);
        results.post(entries);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "close window"));
        settlement.openWeekPool(Settlement.WeekOpening({week: keccak256("soon"), entry: entry}));
        // A session entry isn't a field entry.
        Session memory s = newSession(2);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "not a field entry"));
        settlement.openWeekPool(Settlement.WeekOpening({week: keccak256("x"), entry: s.entry}));
    }

    // ------------------------------------------------------- settling

    /// alice on bot 0, bob on bot 1, carol on bot 2; the final is bots 0 and 1.
    function threeBackers() internal returns (Week memory w, Session memory f) {
        w = newWeek(4);
        betWeek(alice, w, 0, 30 * USDC);
        betWeek(bob, w, 1, 10 * USDC);
        betWeek(carol, w, 2, 10 * USDC);
        f = weekSession(w, 2, w.finalClose, 0);
        anchorFinal(w, f);
    }

    function test_backers_of_the_champion_split_the_pool_less_the_fee() public {
        (Week memory w, Session memory f) = threeBackers();
        // Before the result: nothing yet.
        (uint256 owed,) = settlement.weekClaimable(w.id, alice, f.entry);
        assertEq(owed, 0);
        resolve(f, order(places2(1, 0)));
        // Bot 1 is champion: bob takes the 50 USDC pool less 5%.
        assertEq(claimWeek(bob, w, f.entry), 475 * USDC / 10);
        assertEq(claimWeek(alice, w, f.entry), 0);
        assertEq(claimWeek(carol, w, f.entry), 0);
        assertEq(claimWeek(bob, w, f.entry), 0, "once");
    }

    function test_a_draw_in_the_final_is_a_dead_heat_between_co_champions() public {
        (Week memory w, Session memory f) = threeBackers();
        resolve(f, order(places2(0, 0)));
        (, bytes32[] memory champions) = settlement.weekStanding(w.id, f.entry);
        assertEq(champions.length, 2);
        // 47.5 USDC net, split 30:10 between the co-champions' backers.
        assertEq(claimWeek(alice, w, f.entry), 35_625_000);
        assertEq(claimWeek(bob, w, f.entry), 11_875_000);
        assertEq(claimWeek(carol, w, f.entry), 0);
    }

    function test_nobody_backing_the_champion_refunds_less_the_fee() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 2, 10 * USDC);
        betWeek(bob, w, 3, 10 * USDC);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        anchorFinal(w, f);
        resolve(f, order(places2(0, 1)));
        assertEq(claimWeek(alice, w, f.entry), 95 * USDC / 10);
        assertEq(claimWeek(bob, w, f.entry), 95 * USDC / 10);
    }

    function test_a_one_sided_week_pool_refunds_in_full() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, 10 * USDC);
        betWeek(bob, w, 0, 5 * USDC);
        Session memory f = weekSession(w, 2, w.finalClose, 0);
        anchorFinal(w, f);
        resolve(f, order(places2(0, 1)));
        assertEq(claimWeek(alice, w, f.entry), 10 * USDC);
        assertEq(claimWeek(bob, w, f.entry), 5 * USDC);
    }

    function test_a_void_final_refunds_in_full() public {
        (Week memory w, Session memory f) = threeBackers();
        resolve(f, voidOutcome(1));
        assertEq(claimWeek(alice, w, ""), 30 * USDC, "a refund needs no final entry");
        assertEq(claimWeek(carol, w, f.entry), 10 * USDC);
    }

    function test_a_called_off_week_refunds_at_once() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, 10 * USDC);
        betWeek(bob, w, 1, 10 * USDC);
        assertEq(claimWeek(alice, w, ""), 0, "pending");
        vm.prank(admin);
        results.cancelWeeks(weeksList(w.id));
        assertEq(claimWeek(alice, w, ""), 10 * USDC);
        assertEq(claimWeek(bob, w, ""), 10 * USDC);
    }

    function test_no_final_by_the_final_s_deadline_refunds_with_no_transaction() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, 10 * USDC);
        betWeek(bob, w, 1, 10 * USDC);
        vm.warp(w.finalClose + results.RESULT_DEADLINE() - 1);
        assertEq(claimWeek(alice, w, ""), 0, "not yet");
        vm.warp(w.finalClose + results.RESULT_DEADLINE());
        assertEq(claimWeek(alice, w, ""), 10 * USDC);
        // A bound final with no result by then refunds the same way.
        (Week memory w2, Session memory f2) = threeBackers();
        vm.warp(w2.finalClose + results.RESULT_DEADLINE());
        assertEq(claimWeek(alice, w2, f2.entry), 30 * USDC);
    }

    function test_the_bound_final_s_key_revoked_refunds_at_once() public {
        (Week memory w,) = threeBackers();
        vm.prank(admin);
        results.revokeKey(enclave);
        assertEq(claimWeek(bob, w, ""), 10 * USDC);
    }

    function test_a_settled_claim_needs_the_bound_final_s_entry_and_a_list_skips_what_pays_nothing() public {
        (Week memory w, Session memory f) = threeBackers();
        resolve(f, order(places2(1, 0)));
        Session memory other = newSession(2);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "final entry"));
        settlement.weekStanding(w.id, other.entry);
        bytes32[] memory weeks_ = new bytes32[](3);
        weeks_[0] = w.id;
        weeks_[1] = keccak256("no such week");
        weeks_[2] = w.id;
        bytes[] memory entries = new bytes[](3);
        entries[0] = other.entry;
        entries[1] = "";
        entries[2] = f.entry;
        vm.expectEmit(true, true, false, true);
        emit Settlement.Skipped(w.id, bob, "not the bound final's entry");
        vm.prank(bob);
        settlement.claimWeeks(weeks_, entries);
        assertTrue(settlement.weekClaimed(w.id, bob));
        // A stranger's claimFor pays only the bettor: bob's winnings to bob.
        assertEq(usdc.balanceOf(address(this)), 0);
        bytes[] memory one_ = new bytes[](1);
        one_[0] = f.entry;
        uint256 before = usdc.balanceOf(alice);
        settlement.claimWeeksFor(alice, weeksList(w.id), one_);
        assertEq(usdc.balanceOf(alice), before, "alice backed a loser");
        assertEq(usdc.balanceOf(address(this)), 0);
    }

    // ------------------------------------------------------- fees

    function feeItem(
        Week memory w,
        Session memory f,
        uint256[] memory champs
    ) internal pure returns (Settlement.WeekFeeItem[] memory items) {
        items = new Settlement.WeekFeeItem[](1);
        items[0].week = w.id;
        items[0].finalEntry = f.entry;
        items[0].champions = new Settlement.FieldLeaf[](champs.length);
        items[0].proofs = new bytes32[][](champs.length);
        for (uint256 i = 0; i < champs.length; i++) {
            items[0].champions[i] = w.leaves[champs[i]];
            items[0].proofs[i] = leafProof(w, champs[i]);
        }
    }

    function test_the_week_fee_pays_the_champion_s_frozen_payout_and_the_treasury() public {
        (Week memory w, Session memory f) = threeBackers();
        resolve(f, order(places2(1, 0)));
        uint256[] memory champs = new uint256[](1);
        champs[0] = 1;
        Settlement.WeekFeeItem[] memory items = feeItem(w, f, champs);
        // The wrong leaf: skipped, nothing paid.
        items[0].champions[0] = w.leaves[0];
        settlement.claimWeekFees(items);
        assertEq(usdc.balanceOf(w.leaves[1].payout), 0);
        items = feeItem(w, f, champs);
        uint256 t = usdc.balanceOf(treasury);
        settlement.claimWeekFees(items);
        assertEq(usdc.balanceOf(w.leaves[1].payout), USDC / 2, "1 point of 50 USDC");
        assertEq(usdc.balanceOf(treasury) - t, 2 * USDC, "the other 4 points");
        settlement.claimWeekFees(items);
        assertEq(usdc.balanceOf(treasury) - t, 2 * USDC, "once");
        // Every claim and the fee together are exactly the pool.
        claimWeek(bob, w, f.entry);
        assertEq(usdc.balanceOf(address(settlement)), 0);
    }

    function test_a_dead_heat_splits_the_builder_point_and_a_refunded_week_takes_no_fee() public {
        (Week memory w, Session memory f) = threeBackers();
        resolve(f, order(places2(0, 0)));
        uint256[] memory champs = new uint256[](2);
        champs[0] = 0;
        champs[1] = 1;
        settlement.claimWeekFees(feeItem(w, f, champs));
        assertEq(usdc.balanceOf(w.leaves[0].payout), USDC / 4);
        assertEq(usdc.balanceOf(w.leaves[1].payout), USDC / 4);

        Week memory w2 = newWeek(4, keccak256("w2"));
        betWeek(alice, w2, 0, USDC);
        betWeek(bob, w2, 1, USDC);
        vm.prank(admin);
        results.cancelWeeks(weeksList(w2.id));
        (uint256 total,,) = settlement.weekFee(w2.id, "");
        assertEq(total, 0);
    }

    // ------------------------------------------------------- gas

    function test_gas_of_a_week_bet_with_a_large_field_and_a_binding_anchor() public {
        Week memory w = newWeek(128);
        Settlement.WeekOpening memory o = weekOpening(w);
        Settlement.FieldLeaf memory leaf = w.leaves[77];
        bytes32[] memory proof = leafProof(w, 77);
        vm.prank(alice);
        uint256 g = gasleft();
        settlement.openAndBetWeekFor(o, leaf, proof, USDC, alice);
        emit log_named_uint("first week bet, 128-bot field, gas", g - gasleft());
        leaf = w.leaves[3];
        proof = leafProof(w, 3);
        vm.prank(bob);
        g = gasleft();
        settlement.betWeekFor(w.id, leaf, proof, USDC, bob);
        emit log_named_uint("later week bet, gas", g - gasleft());
        Session memory f = weekSession(w, 6, w.finalClose, 0);
        vm.warp(f.close + results.ANCHOR_MARGIN());
        vm.roll(block.number + 1);
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = finalItem(w, f);
        vm.prank(enclave);
        g = gasleft();
        results.anchor(items);
        emit log_named_uint("final's anchor with its binding, gas", g - gasleft());
    }
}
