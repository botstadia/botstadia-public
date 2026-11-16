// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Settlement} from "../../src/Settlement.sol";
import {StandingOrders} from "../../src/StandingOrders.sol";
import {Base} from "../helpers/Base.sol";

/// Drives bets, fills, results, cancels, claims, fee claims, deposits and
/// withdrawals, on session pools and a week-winner pool, in any order, and
/// keeps the books.
contract Handler is Base {
    Session[] internal sessions;
    address[] internal people;
    bool[] internal resolvedOrCancelled;
    Week internal wk;
    Session internal finalSession;
    bool internal weekDone;

    uint256 public betIn; // through settlement, by anyone
    uint256 public paidOut; // claims and fees out of settlement
    uint256 public deposited;
    uint256 public withdrawn;
    uint256 public filledBets;
    uint256 public fillFees;

    constructor() {
        setUp();
        for (uint256 i = 0; i < 4; i++) {
            address p = bettor(string(abi.encodePacked("p", vm.toString(i))));
            vm.prank(p);
            usdc.approve(address(orders), type(uint256).max);
            people.push(p);
        }
        for (uint256 i = 0; i < 3; i++) {
            sessions.push(newSession(2 + i, uint64(vm.getBlockTimestamp() + 6 hours)));
            resolvedOrCancelled.push(false);
        }
        Week memory w = newWeek(4);
        wk.id = w.id;
        wk.root = w.root;
        wk.post = w.post;
        wk.entry = w.entry;
        wk.poolClose = w.poolClose;
        wk.finalClose = w.finalClose;
        for (uint256 i = 0; i < w.leaves.length; i++) {
            wk.leaves.push(w.leaves[i]);
            wk.encoded.push(w.encoded[i]);
        }
        for (uint256 i = 0; i < w.proof.length; i++) {
            wk.proof.push(w.proof[i]);
        }
    }

    function placeWeekBet(uint256 who, uint256 bot_, uint256 amount) external {
        if (vm.getBlockTimestamp() >= wk.poolClose || weekDone) return;
        amount = bound(amount, USDC, 1000 * USDC);
        betWeek(people[who % people.length], wk, bot_ % 4, amount);
        betIn += amount;
    }

    /// The week ends one of four ways: called off, a void final, a final
    /// with a champion, or a draw.
    function finishWeek(uint256 how) external {
        if (weekDone) return;
        weekDone = true;
        if (how % 4 == 0) {
            vm.prank(admin);
            results.cancelWeeks(one(wk.id));
            return;
        }
        finalSession = weekSession(wk, 2, wk.finalClose, 0);
        anchorFinal(wk, finalSession);
        if (how % 4 == 1) resolve(finalSession, voidOutcome(1));
        else if (how % 4 == 2) resolve(finalSession, order(places2(1, 0)));
        else resolve(finalSession, order(places2(0, 0)));
    }

    function claimWeekOf(uint256 who) external {
        uint256 before = usdc.balanceOf(address(settlement));
        claimWeek(people[who % people.length], wk, finalSession.entry);
        paidOut += before - usdc.balanceOf(address(settlement));
    }

    function claimWeekFee() external {
        (,, bytes32[] memory champions) = settlement.weekFee(wk.id, finalSession.entry);
        Settlement.WeekFeeItem[] memory items = new Settlement.WeekFeeItem[](1);
        items[0].week = wk.id;
        items[0].finalEntry = finalSession.entry;
        items[0].champions = new Settlement.FieldLeaf[](champions.length);
        items[0].proofs = new bytes32[][](champions.length);
        for (uint256 i = 0; i < champions.length; i++) {
            uint256 at = 0;
            while (wk.leaves[at].digest != champions[i]) at++;
            items[0].champions[i] = wk.leaves[at];
            items[0].proofs[i] = leafProof(wk, at);
        }
        uint256 before = usdc.balanceOf(address(settlement));
        settlement.claimWeekFees(items);
        paidOut += before - usdc.balanceOf(address(settlement));
    }

    function settlementAddress() external view returns (address) {
        return address(settlement);
    }

    function ordersAddress() external view returns (address) {
        return address(orders);
    }

    function balanceOf(address who) external view returns (uint256) {
        return usdc.balanceOf(who);
    }

    function placeBet(uint256 who, uint256 sessionSeed, uint256 entrant, uint256 amount) external {
        Session memory s = sessions[sessionSeed % sessions.length];
        if (vm.getBlockTimestamp() >= s.close || resolvedOrCancelled[sessionSeed % sessions.length]) return;
        amount = bound(amount, USDC, 1000 * USDC);
        bet(people[who % people.length], s, entrant % s.spec.digests.length, amount);
        betIn += amount;
    }

    function depositAndOrder(uint256 who, uint256 amount, uint256 entrant) external {
        address p = people[who % people.length];
        amount = bound(amount, USDC, 1000 * USDC);
        vm.startPrank(p);
        orders.deposit(amount);
        orders.setOrder(bot(entrant % 2), USDC + amount % (5 * USDC), 2);
        vm.stopPrank();
        deposited += amount;
    }

    function fill(uint256 who, uint256 sessionSeed, uint256 entrant) external {
        uint256 idx = sessionSeed % sessions.length;
        Session memory s = sessions[idx];
        if (vm.getBlockTimestamp() >= s.close || resolvedOrCancelled[idx]) return;
        address p = people[who % people.length];
        StandingOrders.FillItem[] memory items = new StandingOrders.FillItem[](1);
        items[0] = StandingOrders.FillItem(p, bot(entrant % 2), s.id, abi.encode(opening(s)));
        uint256 before = usdc.balanceOf(address(settlement));
        uint256 keeperBefore = usdc.balanceOf(address(this));
        orders.fill(items, address(this));
        uint256 placed = usdc.balanceOf(address(settlement)) - before;
        filledBets += placed;
        betIn += placed;
        fillFees += usdc.balanceOf(address(this)) - keeperBefore;
    }

    function withdraw(uint256 who, uint256 amount) external {
        address p = people[who % people.length];
        uint256 bal = orders.balances(p);
        if (bal == 0) return;
        amount = bound(amount, 1, bal);
        vm.prank(p);
        orders.withdraw(amount);
        withdrawn += amount;
    }

    function finish(uint256 sessionSeed, uint256 how, uint256 winner) external {
        uint256 idx = sessionSeed % sessions.length;
        if (resolvedOrCancelled[idx]) return;
        Session memory s = sessions[idx];
        resolvedOrCancelled[idx] = true;
        if (how % 4 == 0) {
            vm.prank(admin);
            results.cancel(one(s.id));
            return;
        }
        if (vm.getBlockTimestamp() < s.close + results.ANCHOR_MARGIN()) {
            vm.warp(s.close + results.ANCHOR_MARGIN());
        }
        anchorAs(enclave, s);
        uint256 n = s.spec.digests.length;
        uint8[] memory places = new uint8[](n);
        for (uint256 i = 0; i < n; i++) {
            places[i] = i == winner % n ? 0 : uint8(1 + i);
        }
        if (how % 4 == 1) resolve(s, voidOutcome(3));
        else resolve(s, order(places));
    }

    function claim(uint256 who, uint256 sessionSeed) external {
        Session memory s = sessions[sessionSeed % sessions.length];
        address p = people[who % people.length];
        uint256 before = usdc.balanceOf(address(settlement));
        vm.prank(p);
        settlement.claim(one(s.id));
        paidOut += before - usdc.balanceOf(address(settlement));
    }

    function claimFee(uint256 sessionSeed) external {
        Session memory s = sessions[sessionSeed % sessions.length];
        Settlement.FeeItem[] memory items = new Settlement.FeeItem[](1);
        items[0] = Settlement.FeeItem(s.id, s.spec.payouts);
        uint256 before = usdc.balanceOf(address(settlement));
        settlement.claimFees(items);
        paidOut += before - usdc.balanceOf(address(settlement));
    }

    /// What settlement still owes on every pool, at most: each pool's total
    /// less what has been paid out on it is covered by the balance.
    function outstanding() external view returns (uint256 sum) {
        for (uint256 i = 0; i < sessions.length; i++) {
            (,,,, uint128 total) = settlement.pools(sessions[i].id);
            sum += total;
        }
        (,,,, uint128 weekTotal,) = settlement.weekPools(wk.id);
        sum += weekTotal;
    }
}

/// Escrow and orders conservation (MONEY.md#settlement,
/// MONEY.md#standing-orders), under any sequence of actions.
contract EscrowInvariantTest is Base {
    Handler handler;

    function setUp() public override {
        handler = new Handler();
        targetContract(address(handler));
        bytes4[] memory selectors = new bytes4[](11);
        selectors[0] = Handler.placeBet.selector;
        selectors[1] = Handler.depositAndOrder.selector;
        selectors[2] = Handler.fill.selector;
        selectors[3] = Handler.withdraw.selector;
        selectors[4] = Handler.finish.selector;
        selectors[5] = Handler.claim.selector;
        selectors[6] = Handler.claimFee.selector;
        selectors[7] = Handler.placeWeekBet.selector;
        selectors[8] = Handler.finishWeek.selector;
        selectors[9] = Handler.claimWeekOf.selector;
        selectors[10] = Handler.claimWeekFee.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
    }

    /// Settlement holds exactly what was bet less what it paid out, and never
    /// paid out more than was bet.
    function invariant_settlement_holds_bets_less_payouts() public view {
        assertEq(handler.balanceOf(handler.settlementAddress()) + handler.paidOut(), handler.betIn());
        assertLe(handler.paidOut(), handler.outstanding());
    }

    /// Deposits equal balances plus bets placed plus fill fees plus withdrawals.
    function invariant_orders_conserve_deposits() public view {
        assertEq(
            handler.deposited(),
            handler.balanceOf(handler.ordersAddress()) + handler.filledBets() + handler.fillFees()
                + handler.withdrawn()
        );
    }
}
