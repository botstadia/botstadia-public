// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ReceiptClaim, ReceiptClaimLib} from "risc0/IRiscZeroVerifier.sol";
import {RiscZeroMockVerifier} from "risc0/test/RiscZeroMockVerifier.sol";

import {Merkle} from "../../src/Merkle.sol";
import {Results} from "../../src/Results.sol";
import {Settlement} from "../../src/Settlement.sol";
import {StandingOrders} from "../../src/StandingOrders.sol";
import {MockUSDC} from "../../src/local/MockUSDC.sol";

/// The whole system as the local deploy makes it: contracts deployed, the
/// image, root, mock verifier and genesis instruction past their 2-day
/// timelock, one SubmitKey approved through the mock verifier, betting on.
abstract contract Base is Test {
    using ReceiptClaimLib for ReceiptClaim;

    bytes32 internal constant PROJECT = keccak256("botstadia-local");
    bytes32 internal constant IMAGE = keccak256("image-1");
    bytes32 internal constant ROOT = keccak256("google-root-1");
    bytes32 internal constant GUEST = keccak256("guest-image-id-1");
    bytes32 internal constant BUNDLE_KEY = keccak256("bundle-key-genesis");
    bytes4 internal constant SELECTOR = 0xb0b0b0b0;
    bytes32 internal constant STATEMENT = keccak256("I am 18 or older and not an excluded person.");
    uint256 internal constant USDC = 1e6;

    address internal admin = makeAddr("admin");
    address internal operator = makeAddr("operator");
    address internal treasury = makeAddr("treasury");
    uint256 internal enclavePk = 0xE1;
    address internal enclave;
    uint256 internal enclave2Pk = 0xE2;
    address internal enclave2;

    MockUSDC internal usdc;
    Results internal results;
    Settlement internal settlement;
    StandingOrders internal orders;
    RiscZeroMockVerifier internal mockVerifier;
    bytes32 internal verifierKey;

    struct Session {
        bytes32 id;
        uint64 post;
        bytes entry;
        bytes32[] proof;
        Settlement.Spec spec;
        uint64 close;
    }

    function setUp() public virtual {
        vm.warp(1_800_000_000);
        enclave = vm.addr(enclavePk);
        enclave2 = vm.addr(enclave2Pk);
        usdc = new MockUSDC();
        results = new Results(admin, operator, PROJECT, new address[](0), new uint256[](0));
        settlement = new Settlement(results, usdc, admin, treasury, STATEMENT);
        orders = new StandingOrders(settlement);
        mockVerifier = new RiscZeroMockVerifier(SELECTOR);
        verifierKey = results.verifierId(address(mockVerifier), GUEST);

        vm.startPrank(admin);
        results.proposeImage(IMAGE);
        results.proposeRoot(ROOT);
        results.proposeVerifier(address(mockVerifier), GUEST);
        results.orderInstruction();
        vm.stopPrank();
        skip(2 days + 1);

        approve(enclave, BUNDLE_KEY);
        vm.prank(admin);
        settlement.setBetting(true);
    }

    // ------------------------------------------------------------ keys

    function journal(
        address key,
        bytes32 bundleKey,
        uint64 instruction,
        uint64 issuedAt
    ) internal view returns (Results.Journal memory) {
        return Results.Journal({
            imageDigest: IMAGE,
            projectId: PROJECT,
            submitKey: key,
            bundleKey: bundleKey,
            chainId: block.chainid,
            results: address(results),
            instruction: instruction,
            issuedAt: issuedAt,
            rootHash: ROOT
        });
    }

    function seal(bytes memory journalBytes) internal pure returns (bytes memory) {
        return abi.encodePacked(SELECTOR, ReceiptClaimLib.ok(GUEST, sha256(journalBytes)).digest());
    }

    function approveWith(Results.Journal memory j) internal {
        bytes memory bytes_ = abi.encode(j);
        bytes memory s = seal(bytes_);
        vm.prank(j.submitKey);
        results.approveKey(verifierKey, s, bytes_);
    }

    function approve(address key, bytes32 bundleKey) internal {
        approveWith(journal(key, bundleKey, 1, uint64(vm.getBlockTimestamp())));
    }

    // -------------------------------------------------------- sessions

    function digests(uint256 n) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = bytes32(uint256(keccak256("bot")) / 1024 + i + 1);
        }
    }

    function bot(uint256 i) internal pure returns (bytes32) {
        return digests(i + 1)[i];
    }

    function spec(uint256 n, bytes32 nonce) internal view returns (Settlement.Spec memory s) {
        s.digests = digests(n);
        s.payouts = new address[](n);
        for (uint256 i = 0; i < n; i++) {
            s.payouts[i] = address(uint160(0xB000 + i));
        }
        s.arena = keccak256("arena-1");
        s.chainId = block.chainid;
        s.results = address(results);
        s.nonce = nonce;
        s.budgetVersion = 1;
    }

    function sessionEntry(Settlement.Spec memory s, uint64 close) internal view returns (bytes memory) {
        return sessionEntry(s, close, LEAGUE_WEEK, 1);
    }

    /// The week every plain test session belongs to, as a round-1 league session.
    bytes32 internal constant LEAGUE_WEEK = keccak256("league-week");

    function sessionEntry(
        Settlement.Spec memory s,
        uint64 close,
        bytes32 week,
        uint32 round
    ) internal view returns (bytes memory) {
        return abi.encode(uint8(3), settlement.specHash(s), s.digests, close, week, round);
    }

    /// Post `specs` in one post, each closing at `close`, with two other
    /// entries (an arena version and a rating version) beside them so proofs
    /// have depth.
    function postSessions(
        Settlement.Spec[] memory specs,
        uint64 close
    ) internal returns (Session[] memory out) {
        bytes[] memory entries = new bytes[](specs.length + 2);
        entries[0] = abi.encode(uint8(1), keccak256("arena-1"), keccak256("racing"), uint32(1));
        entries[1] = abi.encode(uint8(2), uint32(1));
        for (uint256 i = 0; i < specs.length; i++) {
            entries[i + 2] = sessionEntry(specs[i], close);
        }
        vm.prank(operator);
        uint64 number = results.post(entries);
        out = new Session[](specs.length);
        for (uint256 i = 0; i < specs.length; i++) {
            out[i] = Session({
                id: keccak256(entries[i + 2]),
                post: number,
                entry: entries[i + 2],
                proof: proofFor(entries, i + 2),
                spec: specs[i],
                close: close
            });
        }
    }

    function newSession(uint256 n) internal returns (Session memory) {
        return newSession(n, uint64(vm.getBlockTimestamp() + 6 hours));
    }

    function newSession(uint256 n, uint64 close) internal returns (Session memory) {
        Settlement.Spec[] memory specs = new Settlement.Spec[](1);
        specs[0] = spec(n, keccak256(abi.encode("nonce", vm.getBlockTimestamp(), gasleft())));
        return postSessions(specs, close)[0];
    }

    function opening(Session memory s) internal pure returns (Settlement.Opening memory) {
        return Settlement.Opening({post: s.post, entry: s.entry, proof: s.proof, spec: s.spec});
    }

    /// A Merkle proof for entry `index`, built the way Merkle.root builds the tree.
    function proofFor(bytes[] memory entries, uint256 index) internal pure returns (bytes32[] memory) {
        uint256 n = entries.length;
        bytes32[] memory level = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            level[i] = Merkle.leaf(keccak256(entries[i]));
        }
        bytes32[] memory proof = new bytes32[](64);
        uint256 depth;
        uint256 at = index;
        while (n > 1) {
            uint256 sibling = at ^ 1;
            if (sibling < n) proof[depth++] = level[sibling];
            uint256 next = (n + 1) / 2;
            for (uint256 i = 0; i < next; i++) {
                uint256 left = 2 * i;
                level[i] = left + 1 < n ? Merkle.node(level[left], level[left + 1]) : level[left];
            }
            n = next;
            at /= 2;
        }
        bytes32[] memory out = new bytes32[](depth);
        for (uint256 i = 0; i < depth; i++) {
            out[i] = proof[i];
        }
        return out;
    }

    // ------------------------------------------------- anchors, results

    function anchorItem(Session memory s) internal pure returns (Results.AnchorItem memory) {
        return Results.AnchorItem({post: s.post, entry: s.entry, proof: s.proof, fieldEntry: ""});
    }

    struct AnchorSeen {
        bytes32 hash;
        uint64 number;
    }

    /// The anchor hash and block of each session anchored through these
    /// helpers, as an enclave knows them from its own transaction.
    mapping(bytes32 => AnchorSeen) internal anchorOf;

    function anchorAs(address key, Session memory s) internal {
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = anchorItem(s);
        anchorBatch(key, items);
    }

    function anchorBatch(address key, Results.AnchorItem[] memory items) internal {
        bytes32 hash = blockhash(vm.getBlockNumber() - 1);
        uint64 number = uint64(vm.getBlockNumber() - 1);
        vm.prank(key);
        results.anchor(items);
        for (uint256 i = 0; i < items.length; i++) {
            bytes32 id = keccak256(items[i].entry);
            (address bound,,,) = results.anchors(id);
            if (bound == key && anchorOf[id].number == 0) anchorOf[id] = AnchorSeen(hash, number);
        }
    }

    /// Warp past close and the anchor margin, then anchor with the enclave.
    function closeAndAnchor(Session memory s) internal {
        vm.warp(s.close + results.ANCHOR_MARGIN());
        vm.roll(block.number + 1);
        anchorAs(enclave, s);
    }

    function order(uint8[] memory places) internal pure returns (bytes memory) {
        return abi.encode(uint8(0), uint16(0), places);
    }

    function voidOutcome(uint16 reason) internal pure returns (bytes memory) {
        return abi.encode(uint8(1), reason, new uint8[](0));
    }

    function places2(uint8 a, uint8 b) internal pure returns (uint8[] memory p) {
        p = new uint8[](2);
        p[0] = a;
        p[1] = b;
    }

    function resultItem(
        Session memory s,
        bytes memory outcome
    ) internal view returns (Results.ResultItem memory) {
        AnchorSeen memory seen = anchorOf[s.id];
        return Results.ResultItem({
            session: s.id,
            anchor: seen.hash,
            anchorBlock: seen.number,
            specHash: settlement.specHash(s.spec),
            outcome: outcome,
            seed: keccak256("seed"),
            archiveHash: keccak256("archive"),
            signature: ""
        });
    }

    function submitAs(address key, Results.ResultItem memory item) internal {
        Results.ResultItem[] memory items = new Results.ResultItem[](1);
        items[0] = item;
        vm.prank(key);
        results.submit(items, new Results.RatingItem[](0));
    }

    /// Wait out the 15-minute minimum and record `outcome` from the enclave.
    function resolve(Session memory s, bytes memory outcome) internal {
        skip(results.RESULT_MIN_AGE());
        submitAs(enclave, resultItem(s, outcome));
    }

    // ------------------------------------------------------------ bets

    function bettor(string memory name) internal returns (address who) {
        who = makeAddr(name);
        usdc.mint(who, 1_000_000 * USDC);
        vm.startPrank(who);
        usdc.approve(address(settlement), type(uint256).max);
        settlement.makeStatement();
        vm.stopPrank();
    }

    function bet(address who, Session memory s, uint256 entrant, uint256 amount) internal {
        bool open = settlement.poolOpen(s.id);
        Settlement.Opening memory o = opening(s);
        vm.prank(who);
        if (open) {
            settlement.betFor(s.id, s.spec.digests[entrant], amount, who);
        } else {
            settlement.openAndBetFor(o, s.spec.digests[entrant], amount, who);
        }
    }

    function one(bytes32 id) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = id;
    }

    function claimOne(address who, Session memory s) internal returns (uint256 paid) {
        uint256 before = usdc.balanceOf(who);
        vm.prank(who);
        settlement.claim(one(s.id));
        paid = usdc.balanceOf(who) - before;
    }

    // ----------------------------------------------------------- weeks

    struct Week {
        bytes32 id;
        Settlement.FieldLeaf[] leaves;
        bytes[] encoded;
        bytes32 root;
        uint64 post;
        bytes entry;
        bytes32[] proof;
        uint64 poolClose;
        uint64 finalClose;
    }

    /// A field of `n` bots (the first `n` of `digests`), each its own group,
    /// paying 0xC000 + i.
    function fieldLeaves(uint256 n) internal pure returns (Settlement.FieldLeaf[] memory leaves) {
        bytes32[] memory d = digests(n);
        leaves = new Settlement.FieldLeaf[](n);
        for (uint256 i = 0; i < n; i++) {
            leaves[i] = Settlement.FieldLeaf({
                digest: d[i],
                payout: address(uint160(0xC000 + i)),
                mu: int64(int256(25_000_000 - i * 100_000)),
                sigma: 8_333_333,
                group: keccak256(abi.encode("group", i))
            });
        }
    }

    function fieldEntry(
        bytes32 id,
        bytes32 root,
        uint64 poolClose,
        uint64 finalClose
    ) internal pure returns (bytes memory) {
        return fieldEntry(id, root, poolClose, finalClose, true);
    }

    function fieldEntry(
        bytes32 id,
        bytes32 root,
        uint64 poolClose,
        uint64 finalClose,
        bool championship
    ) internal pure returns (bytes memory) {
        return abi.encode(
            uint8(4),
            id,
            keccak256("racing/1"),
            keccak256("arena-1"),
            uint32(1),
            uint32(1),
            uint32(1),
            root,
            poolClose,
            finalClose,
            championship,
            uint32(5)
        );
    }

    /// Post a week's field entry for `n` bots: its pool closing in 4 days,
    /// its final in 5.
    function newWeek(uint256 n) internal returns (Week memory w) {
        return newWeek(n, keccak256(abi.encode("week", vm.getBlockTimestamp(), n)));
    }

    function newWeek(uint256 n, bytes32 id) internal returns (Week memory w) {
        return newWeek(n, id, true);
    }

    function newWeek(uint256 n, bytes32 id, bool championship) internal returns (Week memory w) {
        w.id = id;
        w.leaves = fieldLeaves(n);
        w.encoded = new bytes[](n);
        bytes32[] memory hashes = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            w.encoded[i] = abi.encode(w.leaves[i]);
            hashes[i] = keccak256(w.encoded[i]);
        }
        w.root = Merkle.root(hashes);
        w.poolClose = uint64(vm.getBlockTimestamp() + 4 days);
        w.finalClose = uint64(vm.getBlockTimestamp() + 5 days);
        w.entry = fieldEntry(w.id, w.root, w.poolClose, w.finalClose, championship);
        bytes[] memory entries = new bytes[](2);
        entries[0] = w.entry;
        entries[1] = abi.encode(uint8(5), uint32(1));
        vm.prank(operator);
        w.post = results.post(entries);
        w.proof = proofFor(entries, 0);
    }

    function leafProof(Week memory w, uint256 i) internal pure returns (bytes32[] memory) {
        return proofFor(w.encoded, i);
    }

    function weekOpening(Week memory w) internal pure returns (Settlement.WeekOpening memory) {
        return Settlement.WeekOpening({week: w.id, entry: w.entry});
    }

    /// Post a session of the week's first `n` bots in `round` (0: the final).
    /// A final is posted once the week-winner pool has closed, as the keeper
    /// does: the contract binds no final posted earlier.
    function weekSession(
        Week memory w,
        uint256 n,
        uint64 close,
        uint32 round
    ) internal returns (Session memory out) {
        if (round == 0 && vm.getBlockTimestamp() < w.poolClose) vm.warp(w.poolClose);
        return weekSessionNow(w, n, close, round);
    }

    function weekSessionNow(
        Week memory w,
        uint256 n,
        uint64 close,
        uint32 round
    ) internal returns (Session memory out) {
        Settlement.Spec memory sp = spec(n, keccak256(abi.encode("week-session", w.id, round, close)));
        bytes[] memory entries = new bytes[](2);
        entries[0] = sessionEntry(sp, close, w.id, round);
        entries[1] = abi.encode(uint8(2), uint32(1));
        vm.prank(operator);
        uint64 number = results.post(entries);
        out = Session({
            id: keccak256(entries[0]),
            post: number,
            entry: entries[0],
            proof: proofFor(entries, 0),
            spec: sp,
            close: close
        });
    }

    /// The final's anchor item, carrying its week's field entry.
    function finalItem(
        Week memory w,
        Session memory f
    ) internal pure returns (Results.AnchorItem memory item) {
        item = anchorItem(f);
        item.fieldEntry = w.entry;
    }

    /// Warp past the final's close and anchor it with its field entry.
    function anchorFinal(Week memory w, Session memory f) internal {
        if (vm.getBlockTimestamp() < f.close + results.ANCHOR_MARGIN()) {
            vm.warp(f.close + results.ANCHOR_MARGIN());
        }
        vm.roll(block.number + 1);
        Results.AnchorItem[] memory items = new Results.AnchorItem[](1);
        items[0] = finalItem(w, f);
        anchorBatch(enclave, items);
    }

    function betWeek(address who, Week memory w, uint256 bot_, uint256 amount) internal {
        Settlement.FieldLeaf memory leaf = w.leaves[bot_];
        bytes32[] memory proof = leafProof(w, bot_);
        bool open = settlement.weekPoolOpen(w.id);
        vm.prank(who);
        if (open) {
            settlement.betWeekFor(w.id, leaf, proof, amount, who);
        } else {
            settlement.openAndBetWeekFor(weekOpening(w), leaf, proof, amount, who);
        }
    }

    function claimWeek(address who, Week memory w, bytes memory finalEntry) internal returns (uint256 paid) {
        bytes32[] memory weeks_ = new bytes32[](1);
        weeks_[0] = w.id;
        bytes[] memory entries = new bytes[](1);
        entries[0] = finalEntry;
        uint256 before = usdc.balanceOf(who);
        vm.prank(who);
        settlement.claimWeeks(weeks_, entries);
        paid = usdc.balanceOf(who) - before;
    }
}
