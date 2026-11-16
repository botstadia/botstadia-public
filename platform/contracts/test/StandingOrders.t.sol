// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Settlement} from "../src/Settlement.sol";
import {StandingOrders} from "../src/StandingOrders.sol";
import {Base} from "./helpers/Base.sol";

/// BEFORE-PROD "Standing orders" (MONEY.md#standing-orders).
contract StandingOrdersTest is Base {
    address owner;
    address keeper;
    address stranger;
    address other;

    function setUp() public override {
        super.setUp();
        owner = bettor("owner");
        other = bettor("other");
        keeper = makeAddr("keeper");
        stranger = makeAddr("stranger");
        vm.startPrank(owner);
        usdc.approve(address(orders), type(uint256).max);
        orders.deposit(20 * USDC);
        vm.stopPrank();
    }

    function setOrder(bytes32 botDigest, uint256 amount, uint32 cap) internal {
        vm.prank(owner);
        orders.setOrder(botDigest, amount, cap);
    }

    function item(
        Session memory s,
        bytes32 botDigest,
        bool withOpening
    ) internal pure returns (StandingOrders.FillItem memory) {
        return StandingOrders.FillItem({
            owner: address(0),
            bot: botDigest,
            session: s.id,
            opening: withOpening ? abi.encode(opening(s)) : bytes("")
        });
    }

    function fillOne(
        address filler,
        address who,
        Session memory s,
        bytes32 botDigest,
        bool withOpening
    ) internal {
        StandingOrders.FillItem[] memory items = new StandingOrders.FillItem[](1);
        items[0] = item(s, botDigest, withOpening);
        items[0].owner = who;
        vm.prank(filler);
        orders.fill(items, filler);
    }

    function test_a_standing_order_set_before_any_bet_fills_and_opens_the_pool() public {
        setOrder(bot(0), 5 * USDC, 3);
        Session memory s = newSession(2);
        fillOne(keeper, owner, s, bot(0), true);
        assertEq(settlement.stakes(s.id, owner, 0), 5 * USDC);
        assertEq(orders.balances(owner), 20 * USDC - 5 * USDC - orders.FILL_FEE());
        assertEq(usdc.balanceOf(keeper), orders.FILL_FEE(), "the fee is paid to the filler");
    }

    function test_a_stranger_fills_and_is_paid_the_fee() public {
        setOrder(bot(0), 2 * USDC, 3);
        Session memory s = newSession(2);
        fillOne(stranger, owner, s, bot(0), true);
        assertEq(usdc.balanceOf(stranger), orders.FILL_FEE());
    }

    function test_a_fill_front_run_by_a_stranger_is_skipped_for_the_keeper() public {
        setOrder(bot(0), 2 * USDC, 3);
        Session memory s = newSession(2);
        fillOne(stranger, owner, s, bot(0), true);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(owner, bot(0), s.id, "already filled");
        fillOne(keeper, owner, s, bot(0), true);
        assertEq(settlement.stakes(s.id, owner, 0), 2 * USDC, "never twice");
        assertEq(usdc.balanceOf(keeper), 0);
    }

    function test_stale_opening_data_is_harmless_when_someone_else_opened_the_pool_first() public {
        setOrder(bot(1), 2 * USDC, 3);
        Session memory s = newSession(2);
        bet(other, s, 0, USDC);
        fillOne(keeper, owner, s, bot(1), true);
        assertEq(settlement.stakes(s.id, owner, 1), 2 * USDC);
    }

    function test_a_fill_for_a_wallet_with_no_statement_or_a_bot_not_in_the_session_is_skipped() public {
        address noStatement = makeAddr("no-statement");
        usdc.mint(noStatement, 10 * USDC);
        vm.startPrank(noStatement);
        usdc.approve(address(orders), type(uint256).max);
        orders.deposit(10 * USDC);
        orders.setOrder(bot(0), 2 * USDC, 3);
        orders.setOrder(keccak256("not in it"), 2 * USDC, 3);
        vm.stopPrank();
        Session memory s = newSession(2);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(noStatement, bot(0), s.id, "settlement refused");
        fillOne(keeper, noStatement, s, bot(0), true);
        setOrder(keccak256("not in it"), 2 * USDC, 3);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(owner, keccak256("not in it"), s.id, "settlement refused");
        fillOne(keeper, owner, s, keccak256("not in it"), false);
        assertEq(orders.balances(noStatement), 10 * USDC);
    }

    function test_the_deposit_running_out_skips_the_fill() public {
        setOrder(bot(0), 15 * USDC, 5);
        Session memory a = newSession(2);
        Session memory b = newSession(2, a.close);
        fillOne(keeper, owner, a, bot(0), true);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(owner, bot(0), b.id, "balance");
        fillOne(keeper, owner, b, bot(0), true);
    }

    function test_an_order_reaching_its_daily_cap_waits_for_the_next_utc_day() public {
        setOrder(bot(0), USDC, 1);
        Session memory a = newSession(2, uint64(vm.getBlockTimestamp() + 2 days));
        Session memory b = newSession(2, a.close);
        fillOne(keeper, owner, a, bot(0), true);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(owner, bot(0), b.id, "daily cap");
        fillOne(keeper, owner, b, bot(0), true);
        skip(1 days);
        fillOne(keeper, owner, b, bot(0), true);
        assertEq(settlement.stakes(b.id, owner, 0), USDC);
    }

    function test_a_withdrawal_racing_a_fill_leaves_at_most_the_owner_s_own_bet() public {
        setOrder(bot(0), 5 * USDC, 3);
        Session memory s = newSession(2);
        fillOne(keeper, owner, s, bot(0), true);
        uint256 left = orders.balances(owner);
        vm.prank(owner);
        orders.withdraw(left);
        assertEq(orders.balances(owner), 0);
        Session memory t = newSession(2);
        vm.expectEmit(true, true, true, true);
        emit StandingOrders.Skipped(owner, bot(0), t.id, "balance");
        fillOne(keeper, owner, t, bot(0), true);
    }

    function test_withdrawal_is_never_pausable_even_with_settlement_renounced() public {
        vm.prank(admin);
        settlement.renounce();
        vm.prank(owner);
        orders.withdraw(20 * USDC);
        assertEq(orders.balances(owner), 0);
    }

    function test_the_switch_off_landing_during_a_fill_batch_skips_and_charges_nothing() public {
        setOrder(bot(0), USDC, 3);
        Session memory s = newSession(2);
        vm.prank(admin);
        settlement.setBetting(false);
        fillOne(keeper, owner, s, bot(0), true);
        assertEq(orders.balances(owner), 20 * USDC);
        assertEq(usdc.balanceOf(keeper), 0);
    }

    function test_a_cancel_of_a_session_with_a_pool_and_filled_orders_refunds_the_bet_but_not_the_fee()
        public
    {
        setOrder(bot(0), 4 * USDC, 3);
        Session memory s = newSession(2);
        fillOne(keeper, owner, s, bot(0), true);
        bet(other, s, 1, 4 * USDC);
        vm.prank(admin);
        results.cancel(one(s.id));
        assertEq(claimOne(owner, s), 4 * USDC, "the bet refunds to the owner's wallet, not the deposit");
        assertEq(orders.balances(owner), 20 * USDC - 4 * USDC - orders.FILL_FEE());
    }

    function test_an_old_order_filled_by_a_stranger_into_an_old_contract_pool_after_a_redeploy() public {
        setOrder(bot(0), USDC, 3);
        Session memory s = newSession(2);
        Settlement next = new Settlement(results, usdc, admin, treasury, STATEMENT);
        StandingOrders nextOrders = new StandingOrders(next);
        assertEq(address(nextOrders.SETTLEMENT()), address(next));
        fillOne(stranger, owner, s, bot(0), true);
        assertEq(settlement.stakes(s.id, owner, 0), USDC, "the old contract still takes its fills");
    }

    function test_an_order_below_the_minimum_bet_is_refused_and_zero_clears_it() public {
        vm.prank(owner);
        vm.expectRevert(bytes("below the minimum bet"));
        orders.setOrder(bot(0), USDC - 1, 3);
        setOrder(bot(0), USDC, 3);
        setOrder(bot(0), 0, 0);
        (uint256 amount,) = orders.orders(owner, bot(0));
        assertEq(amount, 0);
    }

    function test_a_pool_opening_fill_at_six_entrants_gas() public {
        setOrder(bot(0), USDC, 3);
        Session memory s = newSession(6);
        StandingOrders.FillItem[] memory items = new StandingOrders.FillItem[](1);
        items[0] = item(s, bot(0), true);
        items[0].owner = owner;
        vm.prank(keeper);
        uint256 g = gasleft();
        orders.fill(items, keeper);
        emit log_named_uint("pool-opening fill, 6 entrants, gas", g - gasleft());
        Session memory t = newSession(6);
        bet(other, t, 1, USDC);
        items[0] = item(t, bot(0), false);
        items[0].owner = owner;
        vm.prank(keeper);
        g = gasleft();
        orders.fill(items, keeper);
        emit log_named_uint("plain fill gas", g - gasleft());
    }
}
