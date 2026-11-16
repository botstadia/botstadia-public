// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";
import {Base} from "./helpers/Base.sol";

/// One test per row of the admin powers table (TRUST.md#admin-powers), plus
/// the timelock's own rules.
contract AdminPowersTest is Base {
    bytes32 constant IMAGE_2 = keccak256("image-2");

    function test_an_image_digest_is_approved_only_after_two_days() public {
        vm.prank(admin);
        results.proposeImage(IMAGE_2);
        assertFalse(results.imageApproved(IMAGE_2));
        skip(2 days - 1);
        assertFalse(results.imageApproved(IMAGE_2));
        skip(1);
        assertTrue(results.imageApproved(IMAGE_2));
    }

    function test_a_google_root_cert_is_added_only_after_two_days() public {
        bytes32 root2 = keccak256("root-2");
        vm.prank(admin);
        results.proposeRoot(root2);
        skip(1 days);
        assertFalse(results.rootApproved(root2));
        skip(1 days);
        assertTrue(results.rootApproved(root2));
    }

    function test_a_verifier_is_approved_only_after_two_days() public {
        vm.prank(admin);
        results.proposeVerifier(address(0xBEEF), keccak256("guest-2"));
        bytes32 id = results.verifierId(address(0xBEEF), keccak256("guest-2"));
        assertFalse(results.verifierApproved(id));
        skip(2 days);
        assertTrue(results.verifierApproved(id));
    }

    function test_a_new_key_instruction_takes_effect_only_after_two_days() public {
        vm.prank(admin);
        uint64 number = results.orderInstruction();
        assertEq(number, 2);
        assertFalse(results.instructionInEffect(2));
        skip(2 days);
        assertTrue(results.instructionInEffect(2));
    }

    function test_an_admin_transfer_takes_two_days_and_can_be_withdrawn_while_pending() public {
        address next = makeAddr("multisig");
        vm.prank(admin);
        results.proposeAdmin(next);
        vm.expectRevert(Results.Timelocked.selector);
        results.acceptAdmin();
        vm.prank(admin);
        results.withdrawAdmin();
        skip(2 days);
        vm.expectRevert(Results.Timelocked.selector);
        results.acceptAdmin();

        vm.prank(admin);
        results.proposeAdmin(next);
        skip(2 days);
        results.acceptAdmin();
        assertEq(results.admin(), next);
    }

    function test_a_pending_action_can_be_withdrawn_but_not_once_in_effect() public {
        vm.startPrank(admin);
        results.proposeImage(IMAGE_2);
        results.withdrawImage(IMAGE_2);
        vm.stopPrank();
        skip(2 days);
        assertFalse(results.imageApproved(IMAGE_2));
        vm.expectRevert(Results.NotPending.selector);
        vm.prank(admin);
        results.withdrawImage(IMAGE);
    }

    function test_revoking_a_proposed_image_bans_it_for_good() public {
        vm.startPrank(admin);
        results.proposeImage(IMAGE_2);
        results.revokeImage(IMAGE_2);
        vm.expectRevert(Results.RevokedForGood.selector);
        results.proposeImage(IMAGE_2);
        vm.stopPrank();
        skip(2 days);
        assertFalse(results.imageApproved(IMAGE_2));
    }

    function test_revocation_is_instant_for_a_key_an_image_a_root_and_a_verifier() public {
        assertTrue(results.keyActive(enclave));
        vm.prank(admin);
        results.revokeKey(enclave);
        assertFalse(results.keyActive(enclave));

        approve(enclave2, BUNDLE_KEY);
        vm.prank(admin);
        results.revokeImage(IMAGE);
        assertFalse(results.keyActive(enclave2), "an image's revocation revokes its keys");
    }

    function test_a_root_or_verifier_revocation_revokes_every_key_it_approved() public {
        approve(enclave2, BUNDLE_KEY);
        uint256 snap = vm.snapshotState();
        vm.prank(admin);
        results.revokeRoot(ROOT);
        assertFalse(results.keyActive(enclave2));
        vm.revertToState(snap);
        vm.prank(admin);
        results.revokeVerifier(verifierKey);
        assertFalse(results.keyActive(enclave2));
        assertFalse(results.keyActive(enclave));
    }

    function test_retiring_an_image_approves_no_new_key_but_keys_it_approved_stand() public {
        vm.prank(admin);
        results.retireImage(IMAGE);
        assertTrue(results.keyActive(enclave));
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(j);
        bytes memory s = seal(b);
        vm.prank(enclave2);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "image"));
        results.approveKey(verifierKey, s, b);
    }

    function test_retiring_needs_an_approved_item() public {
        vm.prank(admin);
        vm.expectRevert(Results.NotApproved.selector);
        results.retireImage(IMAGE_2);
    }

    function test_cancelling_a_list_refunds_unanchored_sessions_and_skips_anchored_ones() public {
        Session memory a = newSession(2);
        Session memory b = newSession(2, a.close);
        closeAndAnchor(a);
        bytes32[] memory list = new bytes32[](2);
        list[0] = a.id;
        list[1] = b.id;
        vm.expectEmit(true, false, false, true);
        emit Results.Rejected(a.id, "already anchored");
        vm.prank(admin);
        results.cancel(list);
        assertFalse(results.cancelled(a.id));
        assertTrue(results.cancelled(b.id));
    }

    function test_calling_off_a_week_refunds_its_pool_at_once_and_leaves_its_sessions_alone() public {
        Week memory w = newWeek(4);
        address a = bettor("a");
        address b = bettor("b");
        betWeek(a, w, 0, USDC);
        betWeek(b, w, 1, USDC);
        Session memory league = weekSession(w, 2, uint64(vm.getBlockTimestamp() + 6 hours), 1);
        bet(a, league, 0, USDC);
        bet(b, league, 1, USDC);
        bytes32[] memory list = new bytes32[](1);
        list[0] = w.id;
        vm.prank(admin);
        results.cancelWeeks(list);
        assertEq(claimWeek(a, w, ""), USDC, "the week pool refunds at once");
        // The league session's own pool runs and settles as usual.
        closeAndAnchor(league);
        resolve(league, order(places2(0, 1)));
        assertEq(claimOne(a, league), 2 * USDC - 2 * USDC * 5 / 100);
    }

    function test_betting_is_switched_off_and_on_by_the_admin_and_the_contract_deploys_off() public {
        Settlement fresh = new Settlement(results, usdc, admin, treasury, STATEMENT);
        assertFalse(fresh.bettingOn());
        vm.prank(admin);
        settlement.setBetting(false);
        assertFalse(settlement.bettingOn());
        vm.prank(makeAddr("stranger"));
        vm.expectRevert(Settlement.NotAdmin.selector);
        settlement.setBetting(true);
    }

    function test_naming_seed_bots_is_instant_and_each_bot_counts_once_on_its_ladder() public {
        bytes32 ladder = keccak256("racing/1");
        bytes32[] memory bots = new bytes32[](2);
        bots[0] = bot(0);
        bots[1] = bot(1);
        bytes32[] memory ladders = new bytes32[](2);
        ladders[0] = ladder;
        ladders[1] = ladder;
        vm.prank(admin);
        results.nameSeedBots(bots, ladders);
        assertTrue(results.seedBots(bot(0)));
        assertEq(results.ladderCount(ladder), 2);
        vm.prank(admin);
        results.nameSeedBots(bots, ladders);
        assertEq(results.ladderCount(ladder), 2, "a later naming changes nothing");
    }

    function test_the_treasury_address_changes_at_once_but_never_to_zero() public {
        address next = makeAddr("treasury-2");
        vm.prank(admin);
        settlement.setTreasury(next);
        assertEq(settlement.treasury(), next);
        vm.prank(admin);
        vm.expectRevert(bytes("zero treasury"));
        settlement.setTreasury(address(0));
    }

    function test_the_operator_address_is_replaced_at_once_and_the_old_one_can_no_longer_post() public {
        address next = makeAddr("operator-2");
        vm.prank(admin);
        results.setOperator(next);
        bytes[] memory entries = new bytes[](1);
        entries[0] = abi.encode(uint8(2), uint32(2));
        vm.prank(operator);
        vm.expectRevert(Results.NotOperator.selector);
        results.post(entries);
        vm.prank(next);
        results.post(entries);
    }

    function test_closing_posts_is_permanent_and_refuses_every_later_post() public {
        vm.prank(admin);
        results.closePosts();
        bytes[] memory entries = new bytes[](1);
        entries[0] = abi.encode(uint8(2), uint32(2));
        vm.prank(operator);
        vm.expectRevert(Results.Closed.selector);
        results.post(entries);
    }

    function test_posts_closed_still_let_sessions_posted_earlier_run() public {
        Session memory s = newSession(2);
        vm.prank(admin);
        results.closePosts();
        closeAndAnchor(s);
        resolve(s, order(places2(0, 1)));
        (bool accepted,,,,) = results.results(s.id);
        assertTrue(accepted);
    }

    function test_renouncing_freezes_every_gate_and_cancels_every_pending_action() public {
        vm.startPrank(admin);
        results.proposeImage(IMAGE_2);
        results.orderInstruction();
        results.renounce();
        vm.stopPrank();
        skip(2 days);
        assertFalse(results.imageApproved(IMAGE_2), "a pending image never takes effect");
        assertFalse(results.instructionInEffect(2), "a pending instruction never takes effect");
        assertTrue(results.imageApproved(IMAGE), "what was in effect stays");
        vm.prank(admin);
        vm.expectRevert(Results.NotAdmin.selector);
        results.revokeKey(enclave);
    }

    function test_renouncing_the_settlement_admin_switches_betting_off_for_good() public {
        vm.prank(admin);
        settlement.renounce();
        assertFalse(settlement.bettingOn());
        vm.prank(admin);
        vm.expectRevert(Settlement.NotAdmin.selector);
        settlement.setBetting(true);
    }

    function test_every_gate_refuses_anyone_but_the_admin() public {
        address stranger = makeAddr("stranger");
        vm.startPrank(stranger);
        vm.expectRevert(Results.NotAdmin.selector);
        results.proposeImage(IMAGE_2);
        vm.expectRevert(Results.NotAdmin.selector);
        results.revokeKey(enclave);
        vm.expectRevert(Results.NotAdmin.selector);
        results.cancel(one(bytes32(0)));
        vm.expectRevert(Results.NotAdmin.selector);
        results.closePosts();
        vm.expectRevert(Results.NotAdmin.selector);
        results.cancelWeeks(one(bytes32(0)));
        vm.expectRevert(Results.NotAdmin.selector);
        results.orderInstruction();
        vm.expectRevert(Results.NotAdmin.selector);
        results.setOperator(stranger);
        vm.expectRevert(Settlement.NotAdmin.selector);
        settlement.setTreasury(stranger);
        vm.stopPrank();
    }
}
