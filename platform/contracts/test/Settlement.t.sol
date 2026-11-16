// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";

import {Base} from "./helpers/Base.sol";
import {ContractWallet} from "./helpers/ContractWallet.sol";

/// BEFORE-PROD "Settlement and pools", and the cases table in
/// MONEY.md#settlement.
contract SettlementTest is Base {
    address alice;
    address bob;
    address carol;

    function setUp() public override {
        super.setUp();
        alice = bettor("alice");
        bob = bettor("bob");
        carol = bettor("carol");
    }

    function feeItems(Session memory s) internal pure returns (Settlement.FeeItem[] memory items) {
        items = new Settlement.FeeItem[](1);
        items[0] = Settlement.FeeItem(s.id, s.spec.payouts);
    }

    // ------------------------------------------------------------ opening

    function test_the_first_bet_opens_the_pool_with_each_entrant_s_digest_and_payout() public {
        Session memory s = newSession(3);
        bet(alice, s, 1, 5 * USDC);
        (uint40 close, uint8 entrants, uint8 sides,, uint128 total) = settlement.pools(s.id);
        assertEq(close, s.close);
        assertEq(entrants, 3);
        assertEq(sides, 1);
        assertEq(total, 5 * USDC);
        assertEq(settlement.entrantIndex(s.id, s.spec.digests[1]), 2);
        assertEq(settlement.payoutsHash(s.id), keccak256(abi.encode(s.spec.payouts)));
        assertEq(settlement.entrantStakes(s.id)[1], 5 * USDC);
    }

    function test_a_first_bet_seconds_before_close_is_accepted_and_at_close_refused() public {
        Session memory s = newSession(2);
        vm.warp(s.close - 1);
        bet(alice, s, 0, USDC);
        vm.warp(s.close);
        vm.prank(bob);
        vm.expectRevert(Settlement.SessionClosed.selector);
        settlement.betFor(s.id, bot(1), USDC, bob);
    }

    function test_a_first_bet_on_a_post_less_than_an_hour_before_close_is_refused() public {
        Session memory s = newSession(2, uint64(vm.getBlockTimestamp() + 1 hours - 1));
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "close window"));
        settlement.openAndBetFor(opening(s), bot(0), USDC, alice);
    }

    function test_a_close_more_than_seven_days_after_its_post_is_refused() public {
        Session memory s = newSession(2, uint64(vm.getBlockTimestamp() + 7 days + 1));
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "close window"));
        settlement.openAndBetFor(opening(s), bot(0), USDC, alice);
    }

    function test_a_spec_that_doesn_t_match_its_entry_is_refused() public {
        Session memory s = newSession(2);
        Settlement.Opening memory o = opening(s);
        o.spec.payouts[0] = makeAddr("thief");
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "spec hash"));
        settlement.openAndBetFor(o, bot(0), USDC, alice);
    }

    function test_digests_out_of_strictly_ascending_order_are_refused() public {
        Settlement.Spec memory sp = spec(2, "desc");
        (sp.digests[0], sp.digests[1]) = (sp.digests[1], sp.digests[0]);
        Settlement.Spec[] memory specs = new Settlement.Spec[](1);
        specs[0] = sp;
        Session memory s = postSessions(specs, uint64(vm.getBlockTimestamp() + 6 hours))[0];
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Settlement.BadOpening.selector, "not strictly ascending"));
        settlement.openAndBetFor(opening(s), sp.digests[0], USDC, alice);
    }

    function test_an_unposted_session_and_a_bot_not_in_it_are_refused() public {
        Session memory s = newSession(2);
        Settlement.Opening memory o = opening(s);
        o.entry = sessionEntry(spec(2, "unposted"), s.close);
        vm.prank(alice);
        vm.expectRevert(Settlement.NotPosted.selector);
        settlement.openAndBetFor(o, bot(0), USDC, alice);
        bet(alice, s, 0, USDC);
        vm.prank(bob);
        vm.expectRevert(Settlement.NotAnEntrant.selector);
        settlement.betFor(s.id, keccak256("not in it"), USDC, bob);
    }

    function test_bets_are_refused_with_betting_off_without_a_statement_below_the_minimum_or_cancelled()
        public
    {
        Session memory s = newSession(2);
        bet(alice, s, 0, USDC);
        address dave = makeAddr("dave");
        usdc.mint(dave, 10 * USDC);
        vm.prank(dave);
        usdc.approve(address(settlement), type(uint256).max);
        vm.prank(dave);
        vm.expectRevert(Settlement.NoStatement.selector);
        settlement.betFor(s.id, bot(0), USDC, dave);
        vm.prank(bob);
        vm.expectRevert(Settlement.BelowMinimum.selector);
        settlement.betFor(s.id, bot(0), USDC - 1, bob);
        vm.prank(admin);
        settlement.setBetting(false);
        vm.prank(bob);
        vm.expectRevert(Settlement.BettingOff.selector);
        settlement.betFor(s.id, bot(0), USDC, bob);
        vm.prank(admin);
        settlement.setBetting(true);
        vm.prank(admin);
        results.cancel(one(s.id));
        vm.prank(bob);
        vm.expectRevert(Settlement.SessionCancelled.selector);
        settlement.betFor(s.id, bot(0), USDC, bob);
    }

    function test_bet_for_from_a_third_wallet_pulls_from_the_caller_and_credits_the_bettor() public {
        Session memory s = newSession(2);
        uint256 before = usdc.balanceOf(carol);
        vm.prank(carol);
        settlement.openAndBetFor(opening(s), bot(0), 3 * USDC, alice);
        assertEq(usdc.balanceOf(carol), before - 3 * USDC);
        assertEq(settlement.stakes(s.id, alice, 0), 3 * USDC);
        assertEq(settlement.stakes(s.id, carol, 0), 0);
    }

    function test_a_batch_bet_with_one_closed_session_places_the_rest_and_reports_the_failure() public {
        Session memory soon = newSession(2, uint64(vm.getBlockTimestamp() + 2 hours));
        Session memory later = newSession(2, uint64(vm.getBlockTimestamp() + 9 hours));
        bet(bob, soon, 1, USDC);
        vm.warp(soon.close + 1);
        Settlement.BetItem[] memory items = new Settlement.BetItem[](2);
        items[0] = Settlement.BetItem(soon.id, bot(0), USDC, alice, "");
        items[1] = Settlement.BetItem(later.id, bot(0), 2 * USDC, alice, abi.encode(opening(later)));
        vm.expectEmit(true, true, false, false);
        emit Settlement.Skipped(soon.id, alice, "");
        vm.prank(alice);
        settlement.betMany(items);
        assertEq(settlement.stakes(later.id, alice, 0), 2 * USDC);
        assertEq(settlement.stakes(soon.id, alice, 0), 0);
    }

    // ---------------------------------------------------------- the cases

    function test_a_normal_win_pays_the_winners_pro_rata_less_the_fee() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 30 * USDC);
        bet(carol, s, 0, 10 * USDC);
        bet(bob, s, 1, 60 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        // Pool 100, fee 5, winners' stake 40.
        assertEq(claimOne(alice, s), 95 * USDC * 30 / 40);
        assertEq(claimOne(carol, s), 95 * USDC * 10 / 40);
        assertEq(claimOne(bob, s), 0);
    }

    function test_a_draw_shares_the_pool_less_the_fee_among_every_backer() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 40 * USDC);
        bet(bob, s, 1, 60 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 0)));
        assertEq(claimOne(alice, s), 38 * USDC);
        assertEq(claimOne(bob, s), 57 * USDC);
    }

    function test_a_tie_for_first_in_a_race_is_a_dead_heat_between_the_tied_bots_backers() public {
        Session memory s = newSession(4);
        bet(alice, s, 0, 20 * USDC);
        bet(bob, s, 1, 30 * USDC);
        bet(carol, s, 2, 50 * USDC);
        closeAndAnchor(s);
        uint8[] memory places = new uint8[](4);
        places[0] = 0;
        places[1] = 0;
        places[2] = 2;
        places[3] = 3;
        resolve(s, order(places));
        // Pool 100 less 5, split 20:30.
        assertEq(claimOne(alice, s), 95 * USDC * 20 / 50);
        assertEq(claimOne(bob, s), 95 * USDC * 30 / 50);
        assertEq(claimOne(carol, s), 0);
    }

    function test_a_one_sided_pool_refunds_in_full_with_no_fee_whatever_the_outcome() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 0, 5 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(1, 0)));
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 5 * USDC);
        (uint256 feeTotal,,) = settlement.fee(s.id);
        assertEq(feeTotal, 0);
    }

    function test_a_race_whose_winner_nobody_backed_refunds_less_the_fee() public {
        Session memory s = newSession(3);
        bet(alice, s, 0, 40 * USDC);
        bet(bob, s, 1, 60 * USDC);
        closeAndAnchor(s);
        uint8[] memory places = new uint8[](3);
        places[0] = 1;
        places[1] = 2;
        places[2] = 0;
        resolve(s, order(places));
        assertEq(claimOne(alice, s), 38 * USDC);
        assertEq(claimOne(bob, s), 57 * USDC);
        (uint256 feeTotal, uint256 builderShare, uint256 mask) = settlement.fee(s.id);
        assertEq(feeTotal, 5 * USDC);
        assertEq(builderShare, 1 * USDC);
        assertEq(mask, 4, "the unbacked winner's builder gets the point");
    }

    function test_a_void_refunds_in_full_with_no_fee() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        resolve(s, voidOutcome(2)); // e.g. entrant-unavailable
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 10 * USDC);
    }

    function test_a_cancel_after_close_and_before_the_anchor_refunds_in_full() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 20 * USDC);
        vm.warp(s.close + 2);
        vm.prank(admin);
        results.cancel(one(s.id));
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 20 * USDC);
    }

    function test_a_revoked_bound_key_refunds_at_once() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 20 * USDC);
        closeAndAnchor(s);
        assertEq(claimOne(alice, s), 0, "nothing yet");
        vm.prank(admin);
        results.revokeKey(enclave);
        assertEq(settlement.claimed(s.id, alice), false, "a pending entry isn't marked claimed");
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 20 * USDC);
    }

    function test_an_image_revoked_before_the_result_lands_refunds_at_once() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 20 * USDC);
        closeAndAnchor(s);
        vm.prank(admin);
        results.revokeImage(IMAGE);
        assertEq(claimOne(bob, s), 20 * USDC);
    }

    function test_a_result_accepted_before_a_revocation_still_settles() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        vm.prank(admin);
        results.revokeKey(enclave);
        assertEq(claimOne(alice, s), 19 * USDC);
    }

    function test_no_result_by_the_deadline_refunds_every_claim_in_full() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 20 * USDC);
        closeAndAnchor(s);
        vm.warp(s.close + results.RESULT_DEADLINE() - 1);
        assertEq(claimOne(alice, s), 0);
        skip(1);
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 20 * USDC);
    }

    function test_a_session_never_anchored_refunds_at_its_deadline() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 20 * USDC);
        vm.warp(s.close + results.RESULT_DEADLINE());
        assertEq(claimOne(bob, s), 20 * USDC);
    }

    // ------------------------------------------------------------- claims

    function test_a_claim_over_a_list_skips_no_result_already_claimed_no_stake_and_losing_entries() public {
        Session memory won = newSession(2);
        Session memory lost = newSession(2, won.close);
        Session memory pending = newSession(2, uint64(won.close + 3 hours));
        Session memory notMine = newSession(2, won.close);
        bet(alice, won, 0, 10 * USDC);
        bet(bob, won, 1, 10 * USDC);
        bet(alice, lost, 0, 10 * USDC);
        bet(bob, lost, 1, 10 * USDC);
        bet(alice, pending, 0, 10 * USDC);
        bet(bob, notMine, 0, 10 * USDC);
        bet(carol, notMine, 1, 10 * USDC);
        vm.warp(won.close + results.ANCHOR_MARGIN());
        Results.AnchorItem[] memory items = new Results.AnchorItem[](3);
        items[0] = anchorItem(won);
        items[1] = anchorItem(lost);
        items[2] = anchorItem(notMine);
        anchorBatch(enclave, items);
        skip(results.RESULT_MIN_AGE());
        submitAs(enclave, resultItem(won, order(places2(0, 1))));
        submitAs(enclave, resultItem(lost, order(places2(1, 0))));
        submitAs(enclave, resultItem(notMine, order(places2(1, 0))));

        bytes32[] memory list = new bytes32[](5);
        list[0] = won.id;
        list[1] = lost.id;
        list[2] = pending.id;
        list[3] = notMine.id;
        list[4] = won.id; // already claimed by then
        uint256 before = usdc.balanceOf(alice);
        vm.prank(alice);
        settlement.claim(list);
        assertEq(usdc.balanceOf(alice) - before, 19 * USDC);
        assertTrue(settlement.claimed(lost.id, alice));
        assertFalse(settlement.claimed(pending.id, alice));
    }

    function test_claim_for_by_a_stranger_pays_only_the_bettor_and_the_bettor_s_own_claim_then_skips()
        public
    {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        address stranger = makeAddr("stranger");
        uint256 before = usdc.balanceOf(alice);
        vm.prank(stranger);
        settlement.claimFor(alice, one(s.id));
        assertEq(usdc.balanceOf(alice) - before, 19 * USDC);
        assertEq(usdc.balanceOf(stranger), 0);
        assertEq(claimOne(alice, s), 0, "skipped, never reverting");
    }

    function test_claims_need_no_statement_and_work_with_betting_off_and_after_a_renounce() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        vm.prank(admin);
        settlement.renounce();
        assertEq(claimOne(alice, s), 19 * USDC);
    }

    // --------------------------------------------------------------- fees

    function test_a_fee_claim_pays_the_treasury_and_the_builder_together() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 50 * USDC);
        bet(bob, s, 1, 50 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(s.spec.payouts[0]), 1 * USDC);
        assertEq(usdc.balanceOf(treasury), 4 * USDC);
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(treasury), 4 * USDC, "claimed once");
    }

    function test_a_dead_heat_splits_the_builder_point_equally_and_a_zero_payout_goes_to_the_treasury()
        public
    {
        Settlement.Spec memory sp = spec(2, "zero payout");
        sp.payouts[1] = address(0);
        Settlement.Spec[] memory specs = new Settlement.Spec[](1);
        specs[0] = sp;
        Session memory s = postSessions(specs, uint64(vm.getBlockTimestamp() + 6 hours))[0];
        bet(alice, s, 0, 50 * USDC);
        bet(bob, s, 1, 50 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 0)));
        Settlement.FeeItem[] memory wrong = feeItems(s);
        wrong[0].payouts = new address[](2);
        vm.expectEmit(true, true, false, true);
        emit Settlement.Skipped(s.id, address(0), "payouts don't match the spec");
        settlement.claimFees(wrong);
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(sp.payouts[0]), USDC / 2);
        assertEq(usdc.balanceOf(treasury), 5 * USDC - USDC / 2);
    }

    function test_refunded_pools_take_no_fee() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 50 * USDC);
        bet(bob, s, 1, 50 * USDC);
        vm.prank(admin);
        results.cancel(one(s.id));
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(treasury), 0);
    }

    function test_a_frozen_treasury_still_pays_the_builder_and_its_share_stays_owed_to_the_next_treasury()
        public
    {
        Session memory s = newSession(2);
        bet(alice, s, 0, 50 * USDC);
        bet(bob, s, 1, 50 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(1, 0)));
        usdc.setFrozen(treasury, true);
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(s.spec.payouts[1]), 1 * USDC);
        assertEq(settlement.treasuryOwed(), 4 * USDC);
        address next = makeAddr("treasury-2");
        vm.prank(admin);
        settlement.setTreasury(next);
        settlement.payTreasury();
        assertEq(usdc.balanceOf(next), 4 * USDC);
        assertEq(settlement.treasuryOwed(), 0);
    }

    function test_a_frozen_builder_is_left_its_share_to_claim() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 50 * USDC);
        bet(bob, s, 1, 50 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        address builder = s.spec.payouts[0];
        usdc.setFrozen(builder, true);
        settlement.claimFees(feeItems(s));
        assertEq(usdc.balanceOf(treasury), 4 * USDC);
        assertEq(settlement.builderOwed(builder), 1 * USDC);
        usdc.setFrozen(builder, false);
        vm.prank(builder);
        settlement.claimBuilderOwed();
        assertEq(usdc.balanceOf(builder), 1 * USDC);
    }

    function test_a_frozen_settlement_contract_moves_nothing_until_unfrozen() public {
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        usdc.setFrozen(address(settlement), true);
        vm.prank(alice);
        vm.expectRevert(bytes("frozen"));
        settlement.claim(one(s.id));
        usdc.setFrozen(address(settlement), false);
        assertEq(claimOne(alice, s), 19 * USDC);
    }

    // --------------------------------------------------------- statements

    function test_a_statement_is_recorded_for_an_eoa_by_anyone_carrying_its_signature() public {
        (address wallet, uint256 pk) = makeAddrAndKey("eoa");
        (uint8 v, bytes32 r, bytes32 sg) = vm.sign(pk, settlement.statementDigest(wallet));
        vm.prank(makeAddr("relayer"));
        settlement.recordStatement(wallet, abi.encodePacked(r, sg, v));
        assertTrue(settlement.statements(wallet));
    }

    function test_a_statement_with_a_wrong_signature_is_refused() public {
        (address wallet,) = makeAddrAndKey("eoa");
        (, uint256 other) = makeAddrAndKey("other");
        (uint8 v, bytes32 r, bytes32 sg) = vm.sign(other, settlement.statementDigest(wallet));
        vm.expectRevert(Settlement.BadSignature.selector);
        settlement.recordStatement(wallet, abi.encodePacked(r, sg, v));
    }

    function test_a_statement_is_recorded_for_a_contract_wallet_through_erc_1271() public {
        (address owner, uint256 pk) = makeAddrAndKey("owner");
        ContractWallet wallet = new ContractWallet(owner);
        (uint8 v, bytes32 r, bytes32 sg) = vm.sign(pk, settlement.statementDigest(address(wallet)));
        settlement.recordStatement(address(wallet), abi.encodePacked(r, sg, v));
        assertTrue(settlement.statements(address(wallet)));
    }

    function test_an_erc_1271_check_is_gas_capped() public {
        (address owner, uint256 pk) = makeAddrAndKey("owner");
        ContractWallet wallet = new ContractWallet(owner);
        wallet.setBurnGas(true);
        (uint8 v, bytes32 r, bytes32 sg) = vm.sign(pk, settlement.statementDigest(address(wallet)));
        vm.expectRevert(Settlement.BadSignature.selector);
        settlement.recordStatement{gas: 1_000_000}(address(wallet), abi.encodePacked(r, sg, v));
    }

    function test_a_testnet_statement_is_useless_on_another_contract() public {
        Settlement other = new Settlement(results, usdc, admin, treasury, STATEMENT);
        (address wallet, uint256 pk) = makeAddrAndKey("eoa");
        (uint8 v, bytes32 r, bytes32 sg) = vm.sign(pk, settlement.statementDigest(wallet));
        vm.expectRevert(Settlement.BadSignature.selector);
        other.recordStatement(wallet, abi.encodePacked(r, sg, v));
    }

    // ----------------------------------------------------------- redeploy

    function test_a_settlement_redeploy_keeps_old_pools_on_their_contract_and_new_ones_on_the_new() public {
        Session memory before = newSession(2);
        bet(alice, before, 0, 10 * USDC);
        Settlement next = new Settlement(results, usdc, admin, treasury, STATEMENT);
        vm.prank(admin);
        next.setBetting(true);
        Session memory afterSwitch = newSession(2);
        vm.startPrank(bob);
        usdc.approve(address(next), type(uint256).max);
        next.makeStatement();
        next.openAndBetFor(opening(afterSwitch), bot(0), 10 * USDC, bob);
        vm.stopPrank();
        // The pre-switch session still bets on the old contract.
        bet(bob, before, 1, 10 * USDC);
        assertTrue(settlement.poolOpen(before.id));
        assertFalse(next.poolOpen(before.id));
        assertTrue(next.poolOpen(afterSwitch.id));
    }
}
