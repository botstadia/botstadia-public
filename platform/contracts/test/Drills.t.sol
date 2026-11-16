// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";
import {StandingOrders} from "../src/StandingOrders.sol";
import {Base} from "./helpers/Base.sol";

/// BEFORE-PROD "Drills: vulnerabilities and redeploys" and the key and root
/// rotations in "Enclaves and keys", as far as the contracts go.
contract DrillsTest is Base {
    address alice;
    address bob;

    function setUp() public override {
        super.setUp();
        alice = bettor("alice");
        bob = bettor("bob");
    }

    function test_a_stolen_admin_key_recovery_on_the_old_settlement_contract() public {
        Session memory settled = newSession(2);
        Session memory open = newSession(2, uint64(settled.close + 1 days));
        bet(alice, settled, 0, 10 * USDC);
        bet(bob, settled, 1, 10 * USDC);
        bet(alice, open, 0, 10 * USDC);
        closeAndAnchor(settled);
        resolve(settled, order(places2(0, 1)));

        // At once and in order: set our treasury, claim our fees, renounce.
        address ours = makeAddr("our-treasury");
        vm.startPrank(admin);
        settlement.setTreasury(ours);
        vm.stopPrank();
        Settlement.FeeItem[] memory fees = new Settlement.FeeItem[](1);
        fees[0] = Settlement.FeeItem(settled.id, settled.spec.payouts);
        settlement.claimFees(fees);
        vm.prank(admin);
        settlement.renounce();
        assertEq(usdc.balanceOf(ours), 8 * USDC / 10, "our 0.8 of the 1 USDC fee");

        // Every gate refuses from then on, and betting is off for good.
        vm.prank(admin);
        vm.expectRevert(Settlement.NotAdmin.selector);
        settlement.setTreasury(admin);
        vm.prank(bob);
        vm.expectRevert(Settlement.BettingOff.selector);
        settlement.betFor(open.id, bot(1), USDC, bob);

        // A fill reverts through betFor, and is skipped.
        vm.startPrank(alice);
        usdc.approve(address(orders), type(uint256).max);
        orders.deposit(5 * USDC);
        orders.setOrder(bot(0), USDC, 1);
        vm.stopPrank();
        StandingOrders.FillItem[] memory items = new StandingOrders.FillItem[](1);
        items[0] = StandingOrders.FillItem(alice, bot(0), open.id, "");
        orders.fill(items, address(this));
        assertEq(orders.balances(alice), 5 * USDC);

        // Claims and withdrawals still work.
        assertEq(claimOne(alice, settled), 19 * USDC);
        vm.prank(alice);
        orders.withdraw(5 * USDC);
    }

    function test_a_stolen_admin_key_migration_on_the_old_results_contract() public {
        Session memory anchored = newSession(2);
        Session memory unanchored = newSession(2, uint64(anchored.close + 2 hours));
        closeAndAnchor(anchored);
        // Cancel every old-contract session not yet anchored, at once, and close posts.
        bytes32[] memory list = new bytes32[](2);
        list[0] = anchored.id;
        list[1] = unanchored.id;
        vm.startPrank(admin);
        results.cancel(list);
        results.closePosts();
        vm.stopPrank();
        assertTrue(results.cancelled(unanchored.id));
        assertFalse(results.cancelled(anchored.id));
        // The anchored session still gets its result before the renounce.
        resolve(anchored, order(places2(0, 1)));
        vm.prank(admin);
        results.renounce();
        vm.prank(admin);
        vm.expectRevert(Results.NotAdmin.selector);
        results.revokeKey(enclave);
    }

    function test_a_root_rotation_then_retiring_the_old_root_leaves_its_keys_standing() public {
        bytes32 root2 = keccak256("google-root-2");
        vm.prank(admin);
        results.proposeRoot(root2);
        skip(2 days);
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.rootHash = root2;
        approveWith(j);
        vm.prank(admin);
        results.retireRoot(ROOT);
        assertTrue(results.keyActive(enclave), "a key approved under the old root stands");
        assertTrue(results.keyActive(enclave2));
        Results.Journal memory k =
            journal(makeAddr("enclave-3"), BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(k);
        bytes memory s = seal(b);
        vm.prank(k.submitKey);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "root"));
        results.approveKey(verifierKey, s, b);
    }

    function test_retiring_an_old_verifier_after_a_new_one_stops_it_approving_new_keys() public {
        vm.prank(admin);
        results.proposeVerifier(address(mockVerifier), keccak256("guest-2"));
        skip(2 days);
        vm.prank(admin);
        results.retireVerifier(verifierKey);
        assertTrue(results.keyActive(enclave), "machines approved through the old one keep their keys");
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(j);
        bytes memory s = seal(b);
        vm.prank(enclave2);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "verifier not approved"));
        results.approveKey(verifierKey, s, b);
    }

    function test_the_runtime_vulnerability_drill_refunds_pooled_sessions_by_revoking_their_keys() public {
        Week memory w = newWeek(4);
        betWeek(alice, w, 0, 5 * USDC);
        betWeek(bob, w, 1, 5 * USDC);
        Session memory s = newSession(2);
        bet(alice, s, 0, 10 * USDC);
        bet(bob, s, 1, 10 * USDC);
        closeAndAnchor(s);
        // Betting off, the week in progress called off, then the vulnerable
        // machines' keys revoked (TRUST.md#a-runtime-vulnerability-that-could-
        // let-a-bot-escape-its-seat).
        bytes32[] memory weeks_ = new bytes32[](1);
        weeks_[0] = w.id;
        vm.startPrank(admin);
        settlement.setBetting(false);
        results.cancelWeeks(weeks_);
        results.revokeKey(enclave);
        vm.stopPrank();
        assertEq(claimOne(alice, s), 10 * USDC);
        assertEq(claimOne(bob, s), 10 * USDC);
        assertEq(claimWeek(alice, w, ""), 5 * USDC, "the week refunds with betting off");
        assertEq(claimWeek(bob, w, ""), 5 * USDC);
    }

    function test_a_new_key_instruction_after_a_loss_is_filled_by_a_fresh_mint() public {
        vm.prank(admin);
        uint64 next = results.orderInstruction();
        skip(2 days);
        approveWith(journal(enclave2, keccak256("new bundle key"), next, uint64(vm.getBlockTimestamp())));
        (, bytes32 held) = results.instructions(next);
        assertEq(held, keccak256("new bundle key"));
        (, bytes32 genesis) = results.instructions(1);
        assertEq(genesis, BUNDLE_KEY, "the old instruction keeps its key");
    }
}
