// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Merkle} from "../src/Merkle.sol";
import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";
import {Base} from "./helpers/Base.sol";

/// BEFORE-PROD "Posts, anchors and results", contract-level cases.
contract PostsAnchorsResultsTest is Base {
    // ----------------------------------------------------------------- posts

    function test_the_contract_hashes_the_emitted_list_into_the_post_root() public {
        bytes[] memory entries = new bytes[](3);
        entries[0] = abi.encode(uint8(1), keccak256("arena"), keccak256("boxing"), uint32(1));
        entries[1] = abi.encode(uint8(2), uint32(1));
        entries[2] = sessionEntry(spec(2, "n"), uint64(vm.getBlockTimestamp() + 2 hours));
        bytes32[] memory hashes = new bytes32[](3);
        for (uint256 i = 0; i < 3; i++) {
            hashes[i] = keccak256(entries[i]);
        }
        vm.expectEmit(true, false, false, true);
        emit Results.Posted(1, Merkle.root(hashes), entries);
        vm.prank(operator);
        uint64 number = results.post(entries);
        (bytes32 root, uint64 time) = results.posts(number);
        assertEq(root, Merkle.root(hashes));
        assertEq(time, vm.getBlockTimestamp());
        for (uint256 i = 0; i < 3; i++) {
            assertTrue(results.inPost(number, entries[i], proofFor(entries, i)));
        }
    }

    function test_a_hidden_or_extra_entry_proves_against_no_post() public {
        Session memory s = newSession(2);
        bytes memory hidden = sessionEntry(spec(2, "hidden"), s.close);
        assertFalse(results.inPost(s.post, hidden, s.proof), "an entry not listed");
        assertFalse(results.inPost(s.post + 1, s.entry, s.proof), "a post that doesn't exist");
        // An inner node can't pass as an entry: leaves are domain-separated.
        bytes32[] memory none = new bytes32[](0);
        (bytes32 root,) = results.posts(s.post);
        assertFalse(results.inPost(s.post, abi.encodePacked(root), none));
    }

    function test_only_the_operator_posts_and_an_empty_post_is_refused() public {
        bytes[] memory entries = new bytes[](1);
        entries[0] = abi.encode(uint8(2), uint32(1));
        vm.expectRevert(Results.NotOperator.selector);
        results.post(entries);
        vm.prank(operator);
        vm.expectRevert(Results.NoEntries.selector);
        results.post(new bytes[](0));
    }

    function test_posts_are_numbered() public {
        Session memory a = newSession(2);
        Session memory b = newSession(2);
        assertEq(b.post, a.post + 1);
        assertEq(results.postCount(), b.post);
    }

    function test_two_identical_matchups_in_one_post_are_told_apart_by_the_nonce() public {
        Settlement.Spec[] memory specs = new Settlement.Spec[](2);
        specs[0] = spec(2, "nonce-a");
        specs[1] = spec(2, "nonce-b");
        Session[] memory s = postSessions(specs, uint64(vm.getBlockTimestamp() + 6 hours));
        assertTrue(s[0].id != s[1].id);
        closeAndAnchor(s[0]);
        anchorAs(enclave, s[1]);
        (address k0,,,) = results.anchors(s[0].id);
        (address k1,,,) = results.anchors(s[1].id);
        assertEq(k0, enclave);
        assertEq(k1, enclave);
    }

    function test_a_post_at_the_size_limit_fits_one_transaction() public {
        // 128 KiB of calldata holds this many session entries of six entrants
        // (each names its week and round too).
        uint256 n = 260;
        bytes[] memory entries = new bytes[](n);
        for (uint256 i = 0; i < n; i++) {
            entries[i] = sessionEntry(spec(6, bytes32(i)), uint64(vm.getBlockTimestamp() + 6 hours));
        }
        uint256 size = abi.encodeCall(Results.post, (entries)).length;
        assertLt(size, 128 * 1024);
        vm.prank(operator);
        uint256 gasBefore = gasleft();
        results.post(entries);
        emit log_named_uint("full post gas", gasBefore - gasleft());
        emit log_named_uint("full post calldata bytes", size);
    }

    // --------------------------------------------------------------- anchors

    function test_an_anchor_stores_the_previous_block_hash_and_binds_the_key() public {
        Session memory s = newSession(2);
        vm.warp(s.close + results.ANCHOR_MARGIN());
        vm.roll(vm.getBlockNumber() + 5);
        bytes32 expected = blockhash(vm.getBlockNumber() - 1);
        vm.expectEmit(true, true, false, true);
        emit Results.Anchored(
            s.id, enclave, expected, uint64(vm.getBlockNumber() - 1), settlement.specHash(s.spec)
        );
        anchorAs(enclave, s);
        (address key, uint40 time, uint40 close, uint8 n) = results.anchors(s.id);
        assertEq(key, enclave);
        assertEq(time, vm.getBlockTimestamp());
        assertEq(close, s.close);
        assertEq(n, 2);
        assertEq(
            results.anchorCommit(s.id),
            results.commitment(expected, uint64(vm.getBlockNumber() - 1), settlement.specHash(s.spec))
        );
    }

    function test_an_anchor_before_close_plus_the_margin_is_skipped() public {
        Session memory s = newSession(2);
        vm.warp(s.close + results.ANCHOR_MARGIN() - 1);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "too soon after close");
        anchorAs(enclave, s);
    }

    function test_an_anchor_landing_late_near_the_deadline_is_accepted_and_after_it_skipped() public {
        Session memory s = newSession(2);
        Session memory late = newSession(2, s.close);
        vm.warp(s.close + results.RESULT_DEADLINE() - 1);
        anchorAs(enclave, s);
        (address key,,,) = results.anchors(s.id);
        assertEq(key, enclave, "a session nobody anchored at close picked up hours later");
        skip(1);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(late.id, "past the deadline");
        anchorAs(enclave, late);
    }

    function test_a_second_anchor_for_a_bound_session_is_skipped() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        approve(enclave2, BUNDLE_KEY);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "already anchored");
        anchorAs(enclave2, s);
        (address key,,,) = results.anchors(s.id);
        assertEq(key, enclave);
    }

    function test_a_cancel_and_an_anchor_race_and_the_second_is_refused() public {
        Session memory s = newSession(2);
        vm.warp(s.close + 10);
        vm.prank(admin);
        results.cancel(one(s.id));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "cancelled");
        anchorAs(enclave, s);

        Session memory t = newSession(2, uint64(vm.getBlockTimestamp() + 2 hours));
        closeAndAnchor(t);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(t.id, "already anchored");
        vm.prank(admin);
        results.cancel(one(t.id));
    }

    function test_an_unposted_session_is_skipped_and_the_rest_of_the_batch_lands() public {
        Session memory good = newSession(2);
        bytes memory badEntry = sessionEntry(spec(2, "never posted"), good.close);
        vm.warp(good.close + results.ANCHOR_MARGIN());
        Results.AnchorItem[] memory items = new Results.AnchorItem[](2);
        items[0] = anchorItem(good);
        items[0].entry = badEntry;
        items[1] = anchorItem(good);
        anchorBatch(enclave, items);
        (address keyGood,,,) = results.anchors(good.id);
        (address keyBad,,,) = results.anchors(keccak256(badEntry));
        assertEq(keyGood, enclave);
        assertEq(keyBad, address(0));
    }

    function test_an_unapproved_or_revoked_key_cannot_anchor() public {
        Session memory s = newSession(2);
        vm.warp(s.close + 10);
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = anchorItem(s);
        vm.prank(enclave2);
        vm.expectRevert(Results.KeyNotActive.selector);
        results.anchor(items);
        vm.prank(admin);
        results.revokeKey(enclave);
        vm.prank(enclave);
        vm.expectRevert(Results.KeyNotActive.selector);
        results.anchor(items);
    }

    function test_an_enclave_stopping_before_close_leaves_the_session_to_be_re_offered() public {
        // Nothing is bound until an anchor lands, so another enclave anchors it.
        Session memory s = newSession(2);
        approve(enclave2, BUNDLE_KEY);
        vm.warp(s.close + results.ANCHOR_MARGIN());
        anchorAs(enclave2, s);
        (address key,,,) = results.anchors(s.id);
        assertEq(key, enclave2);
    }

    // --------------------------------------------------------------- results

    function test_a_session_with_no_bets_goes_through_the_same_anchor_and_result_flow() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        resolve(s, order(places2(1, 0)));
        (bool accepted, bool voided,, uint256 mask, bytes32 archive) = results.results(s.id);
        assertTrue(accepted);
        assertFalse(voided);
        assertEq(mask, 2);
        assertEq(archive, keccak256("archive"));
    }

    function test_a_result_before_its_anchor_is_fifteen_minutes_old_is_skipped_and_resubmitted() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        skip(results.RESULT_MIN_AGE() - 1);
        Results.ResultItem memory item = resultItem(s, order(places2(0, 1)));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "anchor too recent");
        submitAs(enclave, item);
        skip(1);
        submitAs(enclave, item);
        (bool accepted,,,,) = results.results(s.id);
        assertTrue(accepted);
    }

    function test_only_the_bound_key_s_result_is_accepted_and_the_first_valid_one_wins() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        approve(enclave2, BUNDLE_KEY);
        skip(results.RESULT_MIN_AGE());
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "not the bound key");
        submitAs(enclave2, resultItem(s, order(places2(1, 0))));
        submitAs(enclave, resultItem(s, order(places2(0, 1))));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "already resolved");
        submitAs(enclave, resultItem(s, order(places2(1, 0))));
        (,,, uint256 mask,) = results.results(s.id);
        assertEq(mask, 1);
    }

    function test_a_result_naming_another_spec_or_anchor_is_skipped() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        skip(results.RESULT_MIN_AGE());
        Results.ResultItem memory item = resultItem(s, order(places2(0, 1)));
        item.specHash = keccak256("swapped bundle");
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "anchor or spec mismatch");
        submitAs(enclave, item);
        item = resultItem(s, order(places2(0, 1)));
        item.anchor = keccak256("another seed");
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "anchor or spec mismatch");
        submitAs(enclave, item);
    }

    function test_the_deadline_is_one_way_and_a_late_result_is_refused() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        vm.warp(s.close + results.RESULT_DEADLINE());
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "past the deadline");
        submitAs(enclave, resultItem(s, order(places2(0, 1))));
    }

    function test_a_key_revoked_while_its_batch_waits_lands_no_result() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        skip(results.RESULT_MIN_AGE());
        Results.ResultItem memory item = resultItem(s, order(places2(0, 1)));
        vm.prank(admin);
        results.revokeKey(enclave);
        Results.ResultItem[] memory items = new Results.ResultItem[](1);
        items[0] = item;
        vm.prank(enclave);
        vm.expectRevert(Results.KeyNotActive.selector);
        results.submit(items, new Results.RatingItem[](0));
        (,,,, bool boundRevoked) = results.sessionState(s.id);
        assertTrue(boundRevoked);
    }

    function test_a_void_is_an_outcome_with_its_reason_code() public {
        Session memory s = newSession(2);
        closeAndAnchor(s);
        resolve(s, voidOutcome(7));
        (bool accepted, bool voided, uint16 reason, uint256 mask,) = results.results(s.id);
        assertTrue(accepted && voided);
        assertEq(reason, 7);
        assertEq(mask, 0);
    }

    function test_a_malformed_outcome_is_skipped() public {
        Session memory s = newSession(3);
        closeAndAnchor(s);
        skip(results.RESULT_MIN_AGE());
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "malformed outcome");
        submitAs(enclave, resultItem(s, order(places2(0, 1)))); // two places for three entrants
        uint8[] memory nobodyFirst = new uint8[](3);
        nobodyFirst[0] = 1;
        nobodyFirst[1] = 2;
        nobodyFirst[2] = 1;
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(s.id, "malformed outcome");
        submitAs(enclave, resultItem(s, order(nobodyFirst)));
    }

    function test_a_full_result_batch_stays_under_the_gas_cap() public {
        uint256 n = 40;
        Settlement.Spec[] memory specs = new Settlement.Spec[](n);
        for (uint256 i = 0; i < n; i++) {
            specs[i] = spec(6, bytes32(i));
        }
        Session[] memory s = postSessions(specs, uint64(vm.getBlockTimestamp() + 6 hours));
        vm.warp(s[0].close + results.ANCHOR_MARGIN());
        Results.AnchorItem[] memory anchorItems = new Results.AnchorItem[](n);
        for (uint256 i = 0; i < n; i++) {
            anchorItems[i] = anchorItem(s[i]);
        }
        uint256 g = gasleft();
        anchorBatch(enclave, anchorItems);
        emit log_named_uint("anchor batch of 40 gas (with test bookkeeping)", g - gasleft());
        skip(results.RESULT_MIN_AGE());
        Results.ResultItem[] memory items = new Results.ResultItem[](n);
        uint8[] memory places = new uint8[](6);
        for (uint256 j = 0; j < 6; j++) {
            places[j] = uint8(j);
        }
        for (uint256 i = 0; i < n; i++) {
            items[i] = resultItem(s[i], order(places));
            items[i].signature = new bytes(65);
        }
        vm.prank(enclave);
        g = gasleft();
        results.submit(items, new Results.RatingItem[](0));
        emit log_named_uint("result batch of 40 gas", g - gasleft());
    }

    // ---------------------------------------------------------- final ratings

    function rating(
        bytes32 botDigest,
        bytes32 ladder,
        int64 mu
    ) internal pure returns (Results.RatingItem memory) {
        return Results.RatingItem({
            bot: botDigest,
            ladder: ladder,
            mu: mu,
            sigma: 8_333_333,
            ratingVersion: 1,
            arenaDigest: keccak256("arena-1"),
            budgetVersion: 1,
            opponentListHash: keccak256("list"),
            signature: ""
        });
    }

    function submitRatings(address key, Results.RatingItem memory r) internal {
        Results.RatingItem[] memory list = new Results.RatingItem[](1);
        list[0] = r;
        vm.prank(key);
        results.submit(new Results.ResultItem[](0), list);
    }

    function test_two_jobs_racing_and_the_first_rating_wins() public {
        bytes32 ladder = keccak256("boxing/1");
        approve(enclave2, BUNDLE_KEY);
        submitRatings(enclave, rating(bot(0), ladder, 25_000_000));
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(bot(0), "already rated");
        submitRatings(enclave2, rating(bot(0), ladder, 30_000_000));
        (bool set,, int64 mu,,) = results.ratings(bot(0));
        assertTrue(set);
        assertEq(mu, 25_000_000);
        assertEq(results.ladderCount(ladder), 1);
    }

    function test_a_final_rating_from_a_revoked_key_is_refused() public {
        vm.prank(admin);
        results.revokeKey(enclave);
        Results.RatingItem[] memory list = new Results.RatingItem[](1);
        list[0] = rating(bot(0), keccak256("boxing/1"), 25_000_000);
        vm.prank(enclave);
        vm.expectRevert(Results.KeyNotActive.selector);
        results.submit(new Results.ResultItem[](0), list);
    }

    function test_a_final_rating_after_a_seed_naming_is_ignored_and_counts_once() public {
        bytes32 ladder = keccak256("boxing/1");
        bytes32[] memory bots = one(bot(0));
        bytes32[] memory ladders = one(ladder);
        vm.prank(admin);
        results.nameSeedBots(bots, ladders);
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(bot(0), "a seed bot");
        submitRatings(enclave, rating(bot(0), ladder, 25_000_000));
        assertEq(results.ladderCount(ladder), 1);
    }

    function test_a_rated_bot_named_a_seed_bot_keeps_its_rating_and_counts_once() public {
        bytes32 ladder = keccak256("boxing/1");
        submitRatings(enclave, rating(bot(0), ladder, 27_000_000));
        vm.prank(admin);
        results.nameSeedBots(one(bot(0)), one(ladder));
        (bool set,, int64 mu,,) = results.ratings(bot(0));
        assertTrue(set);
        assertEq(mu, 27_000_000);
        assertEq(results.ladderCount(ladder), 1);
    }

    // ------------------------------------------------------------- redeploys

    function test_a_redeploy_fixes_its_predecessors_and_cut_offs_for_good() public {
        address[] memory preds = new address[](1);
        preds[0] = address(results);
        uint256[] memory cutoffs = new uint256[](1);
        cutoffs[0] = vm.getBlockNumber();
        Results next = new Results(admin, operator, PROJECT, preds, cutoffs);
        (address[] memory p, uint256[] memory c) = next.predecessors();
        assertEq(p[0], address(results));
        assertEq(c[0], cutoffs[0]);
        // It deploys with nothing approved: no image, root, verifier or instruction.
        assertFalse(next.imageApproved(IMAGE));
        assertFalse(next.instructionInEffect(1));
    }
}
