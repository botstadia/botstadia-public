// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC1271} from "openzeppelin/interfaces/IERC1271.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

import {ReentrancyGuard} from "openzeppelin/utils/ReentrancyGuard.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {EIP712} from "openzeppelin/utils/cryptography/EIP712.sol";

import {Merkle} from "./Merkle.sol";
import {Results} from "./Results.sol";

/// @title BotStadia settlement contract
/// @notice Pari-mutuel, winner-only pools, escrow and claims
/// (MONEY.md#market-model, MONEY.md#settlement). Two kinds of pool: one per
/// session, and one per week on who becomes champion
/// (MONEY.md#the-week-winner-pool). A session's pool opens with its first
/// bet, which proves the session is in an hourly post; a week's opens the
/// same way with its field entry, and each of its bets proves its bot
/// against the field's root, so no pool stores a field. Claims are
/// computed lazily from the accepted result and the pool's totals: there is
/// no settle transaction, nothing loops over recipients, and claims, refunds
/// and fee claims never depend on a gate or the betting switch.
///
/// The fee, the builder point, the minimum bet, the token, the results
/// contract and the eligibility statement are fixed at deploy. Changing any
/// of them means a new settlement contract.
contract Settlement is EIP712, ReentrancyGuard {
    // --------------------------------------------------------------- constants

    uint256 public constant BPS = 10_000;
    /// 5% of each settled pool.
    uint256 public constant FEE_BPS = 500;
    /// 1 of those 5 points goes to the winning bot's builder.
    uint256 public constant BUILDER_BPS = 100;
    /// 1 USDC (6 decimals).
    uint256 public constant MIN_BET = 1e6;
    /// Gas given to a contract wallet's ERC-1271 check.
    uint256 public constant ERC1271_GAS = 100_000;

    bytes32 public constant STATEMENT_TYPEHASH = keccak256("Statement(address wallet,bytes32 statement)");

    // --------------------------------------------------------------- immutables

    Results public immutable RESULTS;
    IERC20 public immutable TOKEN;
    /// The eligibility statement's hash, fixed at deploy (MONEY.md#eligibility).
    bytes32 public immutable STATEMENT;

    // ------------------------------------------------------------------- types

    /// The session spec (TRUST.md#what-the-enclave-signs), as the first bet
    /// carries it. Its hash is the post entry's spec hash.
    struct Spec {
        bytes32[] digests; // strictly ascending bundle digests
        address[] payouts; // each entrant's builder payout address (zero: treasury)
        bytes32 arena; // the arena digest
        uint256 chainId;
        address results;
        bytes32 nonce;
        uint32 budgetVersion;
    }

    /// What opens a pool: the session's post entry, its proof and its spec.
    struct Opening {
        uint64 post;
        bytes entry;
        bytes32[] proof;
        Spec spec;
    }

    /// A pool, packed into one slot plus a commitment to the payout
    /// addresses, so opening one stays cheap (MONEY.md#the-gas-rule). The
    /// digests and payouts are in the `PoolOpened` event; a fee claim carries
    /// the payouts back.
    struct Pool {
        uint40 close;
        uint8 entrants;
        uint8 sides; // entrants with any stake
        bool feeClaimed;
        uint128 total;
    }

    struct FeeItem {
        bytes32 session;
        address[] payouts; // the spec's payout addresses, as the pool opened with them
    }

    struct BetItem {
        bytes32 session;
        bytes32 bot;
        uint256 amount;
        address bettor;
        bytes opening; // abi.encode(Opening), or empty
    }

    // ------------------------------------------------------------------- state

    address public admin;
    address public treasury;
    bool public bettingOn;
    bool public renounced;
    /// Treasury shares whose transfer failed, paid to the then-current
    /// treasury by any later fee claim.
    uint256 public treasuryOwed;
    /// Builder shares whose transfer failed, claimable by that address.
    mapping(address => uint256) public builderOwed;

    mapping(bytes32 => Pool) public pools;
    mapping(bytes32 => bytes32) public payoutsHash;
    /// entrantIndex[session][bot digest] = index + 1 (0: not an entrant)
    mapping(bytes32 => mapping(bytes32 => uint256)) public entrantIndex;
    /// entrantStake[session][index]
    mapping(bytes32 => mapping(uint256 => uint256)) public entrantStake;
    /// stakes[session][bettor][entrant index]
    mapping(bytes32 => mapping(address => mapping(uint256 => uint256))) public stakes;
    mapping(bytes32 => mapping(address => bool)) public claimed;
    mapping(address => bool) public statements;

    /// A week-winner pool, by week id (MONEY.md#the-week-winner-pool).
    struct WeekPool {
        uint40 poolClose;
        uint40 finalClose;
        uint32 sides; // bots with any stake
        bool feeClaimed;
        uint128 total;
        bytes32 fieldRoot;
    }

    /// One bot's leaf in a week's field, as its field entry commits it.
    struct FieldLeaf {
        bytes32 digest;
        address payout; // frozen at the field's freeze (zero: treasury)
        int64 mu;
        int64 sigma;
        bytes32 group;
    }

    /// What opens a week-winner pool: its week's field entry, as the results
    /// contract fixed it at its first post.
    struct WeekOpening {
        bytes32 week;
        bytes entry;
    }

    /// A week's fee claim: the bound final's entry (its id is its hash) and
    /// each champion's leaf and proof, in the final's entrant order.
    struct WeekFeeItem {
        bytes32 week;
        bytes finalEntry;
        FieldLeaf[] champions;
        bytes32[][] proofs;
    }

    mapping(bytes32 => WeekPool) public weekPools;
    /// weekBotStake[week][bot digest]
    mapping(bytes32 => mapping(bytes32 => uint256)) public weekBotStake;
    /// weekStakes[week][bettor][bot digest]
    mapping(bytes32 => mapping(address => mapping(bytes32 => uint256))) public weekStakes;
    /// weekStaked[week][bettor]: everything the bettor put in that pool.
    mapping(bytes32 => mapping(address => uint256)) public weekStaked;
    mapping(bytes32 => mapping(address => bool)) public weekClaimed;

    // ------------------------------------------------------------------ events

    event PoolOpened(bytes32 indexed session, uint64 close, bytes32[] digests, address[] payouts);
    event Bet(
        bytes32 indexed session, address indexed bettor, bytes32 indexed bot, address payer, uint256 amount
    );
    event StatementRecorded(address indexed wallet);
    event Claimed(bytes32 indexed session, address indexed bettor, uint256 amount, bool refund);
    event FeeClaimed(bytes32 indexed session, uint256 treasuryShare, uint256 builderShare);
    event BuilderPaid(bytes32 indexed session, address indexed builder, uint256 amount, bool paid);
    event TreasuryPaid(address indexed treasury, uint256 amount, bool paid);
    event WeekPoolOpened(bytes32 indexed week, uint64 poolClose, uint64 finalClose, bytes32 fieldRoot);
    event WeekBet(
        bytes32 indexed week, address indexed bettor, bytes32 indexed bot, address payer, uint256 amount
    );
    event WeekClaimed(bytes32 indexed week, address indexed bettor, uint256 amount, bool refund);
    event WeekFeeClaimed(bytes32 indexed week, uint256 treasuryShare, uint256 builderShare);
    event BettingSwitched(bool on);
    event TreasuryChanged(address treasury);
    event Renounced();
    /// An entry skipped in a list, with why. The rest go through.
    event Skipped(bytes32 indexed session, address indexed who, string reason);

    // ------------------------------------------------------------------ errors

    error NotAdmin();
    error BettingOff();
    error NoStatement();
    error NotPosted();
    error SessionClosed();
    error SessionCancelled();
    error NotAnEntrant();
    error BelowMinimum();
    error BadOpening(string reason);
    error BadSignature();
    error WeekCalledOff();

    constructor(
        Results results,
        IERC20 token,
        address admin_,
        address treasury_,
        bytes32 statement
    ) EIP712("BotStadia Settlement", "1") {
        require(admin_ != address(0) && treasury_ != address(0), "zero address");
        RESULTS = results;
        TOKEN = token;
        admin = admin_;
        treasury = treasury_;
        STATEMENT = statement;
        // Deploys with betting off (TRUST.md#admin-powers).
    }

    modifier onlyAdmin() {
        if (msg.sender != admin || renounced) revert NotAdmin();
        _;
    }

    // ------------------------------------------------------------ admin gates

    /// Admin powers: "Switch betting off or on", instant.
    function setBetting(bool on) external onlyAdmin {
        bettingOn = on;
        emit BettingSwitched(on);
    }

    /// Admin powers: "Change the treasury address", instant. Never zero.
    function setTreasury(address next) external onlyAdmin {
        require(next != address(0), "zero treasury");
        treasury = next;
        emit TreasuryChanged(next);
    }

    /// Admin powers: "Renounce the admin key", instant, permanent. On the
    /// settlement contract it also switches betting off for good.
    function renounce() external onlyAdmin {
        renounced = true;
        bettingOn = false;
        admin = address(0);
        emit BettingSwitched(false);
        emit Renounced();
    }

    // ------------------------------------------------------------ eligibility

    /// An account wallet makes the statement by calling the contract itself,
    /// which needs no signature (MONEY.md#eligibility).
    function makeStatement() external {
        statements[msg.sender] = true;
        emit StatementRecorded(msg.sender);
    }

    function statementDigest(address wallet) public view returns (bytes32) {
        return _hashTypedDataV4(keccak256(abi.encode(STATEMENT_TYPEHASH, wallet, STATEMENT)));
    }

    /// Anyone carrying the wallet's valid signature can record its statement.
    /// A contract wallet is asked once, through ERC-1271, in a gas-capped
    /// read-only call.
    function recordStatement(address wallet, bytes calldata signature) external nonReentrant {
        bytes32 digest = statementDigest(wallet);
        if (wallet.code.length > 0) {
            (bool ok, bytes memory ret) = wallet.staticcall{gas: ERC1271_GAS}(
                abi.encodeCall(IERC1271.isValidSignature, (digest, signature))
            );
            if (!ok || ret.length != 32 || abi.decode(ret, (bytes4)) != IERC1271.isValidSignature.selector) {
                revert BadSignature();
            }
        } else {
            (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
            if (err != ECDSA.RecoverError.NoError || signer != wallet) revert BadSignature();
        }
        statements[wallet] = true;
        emit StatementRecorded(wallet);
    }

    // ------------------------------------------------------------------ pools

    function specHash(Spec memory spec) public pure returns (bytes32) {
        return keccak256(abi.encode(spec));
    }

    function poolOpen(bytes32 session) public view returns (bool) {
        return pools[session].entrants != 0;
    }

    /// Each entrant's total stake on a session, in spec order.
    function entrantStakes(bytes32 session) public view returns (uint256[] memory out) {
        uint256 n = pools[session].entrants;
        out = new uint256[](n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = entrantStake[session][i];
        }
    }

    /// Open a session's pool from its post entry, proof and spec. Does
    /// nothing if it's already open, so a fill's stale opening data is
    /// harmless. Anyone may call it; a first bet calls it for you.
    function openPool(Opening calldata opening) public returns (bytes32 session) {
        session = keccak256(opening.entry);
        if (poolOpen(session)) return session;
        if (!RESULTS.inPost(opening.post, opening.entry, opening.proof)) revert NotPosted();
        (bytes32 entrySpec, bytes32[] memory digests, uint64 close,,) = RESULTS.decodeSession(opening.entry);
        Spec calldata spec = opening.spec;
        if (specHash(spec) != entrySpec) revert BadOpening("spec hash");
        if (spec.chainId != block.chainid || spec.results != address(RESULTS)) revert BadOpening("binding");
        uint256 n = digests.length;
        if (n < 2 || n > 224 || spec.digests.length != n || spec.payouts.length != n) {
            revert BadOpening("entrants");
        }
        for (uint256 i = 0; i < n; i++) {
            if (spec.digests[i] != digests[i]) revert BadOpening("digests");
            if (i > 0 && digests[i] <= digests[i - 1]) revert BadOpening("not strictly ascending");
        }
        (, uint64 postTime) = RESULTS.posts(opening.post);
        if (close <= block.timestamp) revert SessionClosed();
        if (
            close < uint256(postTime) + RESULTS.CLOSE_MIN_AFTER_POST()
                || close > uint256(postTime) + RESULTS.CLOSE_MAX_AFTER_POST()
        ) revert BadOpening("close window");
        pools[session] =
            Pool({close: uint40(close), entrants: uint8(n), sides: 0, feeClaimed: false, total: 0});
        payoutsHash[session] = keccak256(abi.encode(spec.payouts));
        for (uint256 i = 0; i < n; i++) {
            entrantIndex[session][digests[i]] = i + 1;
        }
        emit PoolOpened(session, close, digests, spec.payouts);
    }

    // ------------------------------------------------------------------- bets

    /// One code path for every bet (MONEY.md#placing-bets): pulls exactly
    /// `amount` from the caller and credits it to `bettor`. Reverts on every
    /// failure.
    function betFor(bytes32 session, bytes32 bot, uint256 amount, address bettor) public nonReentrant {
        _bet(msg.sender, session, bot, amount, bettor);
    }

    function _bet(address payer, bytes32 session, bytes32 bot, uint256 amount, address bettor) internal {
        if (!bettingOn) revert BettingOff();
        if (!statements[bettor]) revert NoStatement();
        if (amount < MIN_BET) revert BelowMinimum();
        Pool storage p = pools[session];
        if (p.entrants == 0) revert NotPosted();
        if (block.timestamp >= p.close) revert SessionClosed();
        if (RESULTS.cancelled(session)) revert SessionCancelled();
        uint256 index = entrantIndex[session][bot];
        if (index == 0) revert NotAnEntrant();
        index -= 1;
        if (entrantStake[session][index] == 0) p.sides += 1;
        entrantStake[session][index] += amount;
        p.total += uint128(amount);
        stakes[session][bettor][index] += amount;
        require(TOKEN.transferFrom(payer, address(this), amount), "transfer");
        emit Bet(session, bettor, bot, payer, amount);
    }

    /// A first bet: open the pool if it isn't yet, then bet.
    function openAndBetFor(Opening calldata opening, bytes32 bot, uint256 amount, address bettor) external {
        bytes32 session = openPool(opening);
        betFor(session, bot, amount, bettor);
    }

    /// A list of bets: one that can no longer be placed is skipped and
    /// reported, the rest go through.
    function betMany(BetItem[] calldata items) external {
        for (uint256 i = 0; i < items.length; i++) {
            BetItem calldata item = items[i];
            if (item.opening.length != 0) {
                try this.openPool(abi.decode(item.opening, (Opening))) {}
                catch {
                    emit Skipped(item.session, item.bettor, "pool could not open");
                    continue;
                }
            }
            try this.betForFrom(msg.sender, item.session, item.bot, item.amount, item.bettor) {}
            catch Error(string memory reason) {
                emit Skipped(item.session, item.bettor, reason);
            } catch (bytes memory) {
                emit Skipped(item.session, item.bettor, "bet refused");
            }
        }
    }

    /// betMany's per-item step, callable only by this contract, so the payer
    /// is the original caller.
    function betForFrom(
        address payer,
        bytes32 session,
        bytes32 bot,
        uint256 amount,
        address bettor
    ) external {
        require(msg.sender == address(this), "internal");
        _bet(payer, session, bot, amount, bettor);
    }

    // ----------------------------------------------------------------- claims

    enum Outcome {
        Pending,
        FullRefund,
        Settled
    }

    /// How a session stands for its pool (MONEY.md#settlement): by the
    /// accepted result if there is one; else a full refund if it's cancelled,
    /// its bound key revoked or its deadline passed; else nothing yet.
    function standing(bytes32 session) public view returns (Outcome, uint256 firstMask) {
        (bool isCancelled, bool resolved, bool voided, uint256 mask, bool boundRevoked) =
            RESULTS.sessionState(session);
        if (resolved) {
            if (voided) return (Outcome.FullRefund, 0);
            return (Outcome.Settled, mask);
        }
        if (isCancelled || boundRevoked) return (Outcome.FullRefund, 0);
        Pool storage p = pools[session];
        if (p.entrants != 0 && block.timestamp >= uint256(p.close) + RESULTS.RESULT_DEADLINE()) {
            return (Outcome.FullRefund, 0);
        }
        return (Outcome.Pending, 0);
    }

    /// What `bettor` would be paid on `session` now, and whether it's a
    /// refund. Zero for nothing yet, nothing staked or a losing stake.
    function claimable(bytes32 session, address bettor) public view returns (uint256 amount, bool refund) {
        Pool storage p = pools[session];
        uint256 n = p.entrants;
        if (n == 0 || claimed[session][bettor]) return (0, false);
        uint256 staked;
        for (uint256 i = 0; i < n; i++) {
            staked += stakes[session][bettor][i];
        }
        if (staked == 0) return (0, false);
        (Outcome outcome, uint256 mask) = standing(session);
        if (outcome == Outcome.Pending) return (0, false);
        if (outcome == Outcome.FullRefund) return (staked, true);
        // A one-sided pool: full refund, no fee, whatever the outcome.
        if (p.sides < 2) return (staked, true);
        uint256 winnersStake;
        uint256 mine;
        for (uint256 i = 0; i < n; i++) {
            if (mask & (uint256(1) << i) != 0) {
                winnersStake += entrantStake[session][i];
                mine += stakes[session][bettor][i];
            }
        }
        // Nobody backed the winner: refunded less the fee.
        if (winnersStake == 0) return (staked * (BPS - FEE_BPS) / BPS, true);
        // A win or a dead heat: winners split the pool less the fee, pro rata.
        uint256 total = p.total;
        uint256 net = total - total * FEE_BPS / BPS;
        return (mine * net / winnersStake, false);
    }

    /// Claim the caller's winnings and refunds on a list of sessions.
    function claim(bytes32[] calldata sessions) external {
        _claim(msg.sender, sessions);
    }

    /// The same claim, callable by anyone, only ever paying the bettor.
    function claimFor(address bettor, bytes32[] calldata sessions) external {
        _claim(bettor, sessions);
    }

    /// An entry that pays nothing, for any reason, is skipped with an event
    /// and never reverts. Only a failed transfer can.
    function _claim(address bettor, bytes32[] calldata sessions) internal nonReentrant {
        uint256 total;
        for (uint256 i = 0; i < sessions.length; i++) {
            bytes32 session = sessions[i];
            if (claimed[session][bettor]) {
                emit Skipped(session, bettor, "already claimed");
                continue;
            }
            if (pools[session].entrants == 0) {
                emit Skipped(session, bettor, "no pool");
                continue;
            }
            (Outcome outcome,) = standing(session);
            if (outcome == Outcome.Pending) {
                emit Skipped(session, bettor, "no result yet");
                continue;
            }
            (uint256 amount, bool refund) = claimable(session, bettor);
            claimed[session][bettor] = true;
            if (amount == 0) {
                emit Skipped(session, bettor, "nothing to pay");
                continue;
            }
            total += amount;
            emit Claimed(session, bettor, amount, refund);
        }
        if (total > 0) require(TOKEN.transfer(bettor, total), "transfer");
    }

    // ------------------------------------------------------------------- fees

    /// The fee on a settled two-sided pool, and its builder share
    /// (MONEY.md#fees-and-the-builder-share). Zero for any refunded pool.
    function fee(bytes32 session)
        public
        view
        returns (uint256 total, uint256 builderShare, uint256 firstMask)
    {
        Pool storage p = pools[session];
        if (p.entrants == 0 || p.feeClaimed) return (0, 0, 0);
        (Outcome outcome, uint256 mask) = standing(session);
        if (outcome != Outcome.Settled || p.sides < 2) return (0, 0, 0);
        return (uint256(p.total) * FEE_BPS / BPS, uint256(p.total) * BUILDER_BPS / BPS, mask);
    }

    /// Claim the fee on many sessions, by anyone. Each pays the builder point
    /// to the payout addresses in the spec (split equally among tied winners;
    /// the zero address's share goes to the treasury) and the rest to the
    /// current treasury. Each transfer is isolated: a failure never blocks
    /// another share or the rest of the batch.
    function claimFees(FeeItem[] calldata items) external nonReentrant {
        uint256 toTreasury = treasuryOwed;
        treasuryOwed = 0;
        for (uint256 i = 0; i < items.length; i++) {
            bytes32 session = items[i].session;
            address[] calldata payouts = items[i].payouts;
            if (keccak256(abi.encode(payouts)) != payoutsHash[session]) {
                emit Skipped(session, address(0), "payouts don't match the spec");
                continue;
            }
            (uint256 total, uint256 builderShare, uint256 mask) = fee(session);
            if (total == 0) {
                emit Skipped(session, address(0), "no fee");
                continue;
            }
            pools[session].feeClaimed = true;
            uint256 winners;
            for (uint256 j = 0; j < payouts.length; j++) {
                if (mask & (uint256(1) << j) != 0) winners++;
            }
            uint256 each = builderShare / winners;
            uint256 paidToBuilders;
            for (uint256 j = 0; j < payouts.length; j++) {
                if (mask & (uint256(1) << j) == 0) continue;
                address builder = payouts[j];
                if (builder == address(0)) continue; // its point goes to the treasury
                paidToBuilders += each;
                bool ok = _tryTransfer(builder, each);
                if (!ok) builderOwed[builder] += each;
                emit BuilderPaid(session, builder, each, ok);
            }
            toTreasury += total - paidToBuilders;
            emit FeeClaimed(session, total - paidToBuilders, paidToBuilders);
        }
        _payTreasury(toTreasury);
    }

    /// Pay any treasury share still owed to the current treasury.
    function payTreasury() external nonReentrant {
        uint256 owed = treasuryOwed;
        treasuryOwed = 0;
        _payTreasury(owed);
    }

    function _payTreasury(uint256 amount) internal {
        if (amount == 0) return;
        bool ok = _tryTransfer(treasury, amount);
        if (!ok) treasuryOwed += amount;
        emit TreasuryPaid(treasury, amount, ok);
    }

    /// A builder whose share's transfer failed claims it here.
    function claimBuilderOwed() external nonReentrant {
        uint256 owed = builderOwed[msg.sender];
        builderOwed[msg.sender] = 0;
        if (owed > 0) require(TOKEN.transfer(msg.sender, owed), "transfer");
    }

    function _tryTransfer(address to, uint256 amount) internal returns (bool) {
        (bool ok, bytes memory ret) = address(TOKEN).call(abi.encodeCall(IERC20.transfer, (to, amount)));
        return ok && (ret.length == 0 || abi.decode(ret, (bool)));
    }

    // ------------------------------------------------------------ week pools

    function weekPoolOpen(bytes32 week) public view returns (bool) {
        return weekPools[week].poolClose != 0;
    }

    /// Open a week-winner pool from its week's field entry. Does nothing if
    /// it's already open. Anyone may call it; a first bet calls it for you.
    /// Only a week with a championship has one.
    function openWeekPool(WeekOpening calldata opening) public returns (bytes32 week) {
        week = opening.week;
        if (weekPoolOpen(week)) return week;
        if (!RESULTS.isFieldEntry(opening.entry)) revert BadOpening("not a field entry");
        (bool ok, Results.FieldEntry memory f) = RESULTS.weekFieldOf(week, opening.entry);
        if (!ok) revert NotPosted();
        if (!f.championship) revert BadOpening("no championship");
        (, uint64 post) = RESULTS.weekFields(week);
        (, uint64 postTime) = RESULTS.posts(post);
        if (f.poolClose <= block.timestamp) revert SessionClosed();
        if (
            f.poolClose < uint256(postTime) + RESULTS.CLOSE_MIN_AFTER_POST()
                || f.finalClose > uint256(postTime) + RESULTS.CLOSE_MAX_AFTER_POST() || f.finalClose < f.poolClose
        ) revert BadOpening("close window");
        weekPools[week] = WeekPool({
            poolClose: uint40(f.poolClose),
            finalClose: uint40(f.finalClose),
            sides: 0,
            feeClaimed: false,
            total: 0,
            fieldRoot: f.fieldRoot
        });
        emit WeekPoolOpened(week, f.poolClose, f.finalClose, f.fieldRoot);
    }

    function leafHash(FieldLeaf calldata leaf) public pure returns (bytes32) {
        return keccak256(abi.encode(leaf));
    }

    /// Back a bot to win the week, proving it's in the field. Pulls exactly
    /// `amount` from the caller and credits it to `bettor`. Reverts on every
    /// failure. Standing orders never call this.
    function betWeekFor(
        bytes32 week,
        FieldLeaf calldata leaf,
        bytes32[] calldata leafProof,
        uint256 amount,
        address bettor
    ) public nonReentrant {
        if (!bettingOn) revert BettingOff();
        if (!statements[bettor]) revert NoStatement();
        if (amount < MIN_BET) revert BelowMinimum();
        WeekPool storage p = weekPools[week];
        if (p.poolClose == 0) revert NotPosted();
        if (block.timestamp >= p.poolClose) revert SessionClosed();
        (bool isCancelled,) = RESULTS.weekState(week);
        if (isCancelled) revert WeekCalledOff();
        if (!Merkle.verify(leafProof, p.fieldRoot, leafHash(leaf))) revert NotAnEntrant();
        bytes32 bot = leaf.digest;
        if (weekBotStake[week][bot] == 0) p.sides += 1;
        weekBotStake[week][bot] += amount;
        weekStakes[week][bettor][bot] += amount;
        weekStaked[week][bettor] += amount;
        p.total += uint128(amount);
        require(TOKEN.transferFrom(msg.sender, address(this), amount), "transfer");
        emit WeekBet(week, bettor, bot, msg.sender, amount);
    }

    /// A first week bet: open the pool if it isn't yet, then bet.
    function openAndBetWeekFor(
        WeekOpening calldata opening,
        FieldLeaf calldata leaf,
        bytes32[] calldata leafProof,
        uint256 amount,
        address bettor
    ) external {
        bytes32 week = openWeekPool(opening);
        betWeekFor(week, leaf, leafProof, amount, bettor);
    }

    /// How a week stands for its pool. A champion is known only from the
    /// bound final's entry (its id is the entry's hash), which the caller
    /// passes; no entry is needed for any refund. `badEntry` is set when a
    /// settled final needs its entry and `finalEntry` isn't it.
    function _weekStanding(
        bytes32 week,
        bytes calldata finalEntry
    ) internal view returns (Outcome outcome, bytes32[] memory champions, bool badEntry) {
        (bool isCancelled, bytes32 finalId) = RESULTS.weekState(week);
        if (isCancelled) return (Outcome.FullRefund, champions, false);
        WeekPool storage p = weekPools[week];
        bool late = block.timestamp >= uint256(p.finalClose) + RESULTS.RESULT_DEADLINE();
        if (finalId == bytes32(0)) {
            return (late ? Outcome.FullRefund : Outcome.Pending, champions, false);
        }
        (bool sessionCancelled, bool resolved, bool voided, uint256 mask, bool boundRevoked) =
            RESULTS.sessionState(finalId);
        if (resolved) {
            if (voided) return (Outcome.FullRefund, champions, false);
            if (keccak256(finalEntry) != finalId) return (Outcome.Pending, champions, true);
            (, bytes32[] memory digests,,,) = RESULTS.decodeSession(finalEntry);
            uint256 count;
            for (uint256 i = 0; i < digests.length; i++) {
                if (mask & (uint256(1) << i) != 0) count++;
            }
            champions = new bytes32[](count);
            uint256 j;
            for (uint256 i = 0; i < digests.length; i++) {
                if (mask & (uint256(1) << i) != 0) champions[j++] = digests[i];
            }
            return (Outcome.Settled, champions, false);
        }
        if (sessionCancelled || boundRevoked || late) return (Outcome.FullRefund, champions, false);
        return (Outcome.Pending, champions, false);
    }

    /// How a week stands: pending, a full refund, or settled with its
    /// champions (several in a dead heat).
    function weekStanding(
        bytes32 week,
        bytes calldata finalEntry
    ) external view returns (Outcome outcome, bytes32[] memory champions) {
        bool badEntry;
        (outcome, champions, badEntry) = _weekStanding(week, finalEntry);
        if (badEntry) revert BadOpening("final entry");
    }

    /// What `bettor` would be paid on a week now, and whether it's a refund.
    /// The same rules as a session's pool (MONEY.md#settlement).
    function weekClaimable(
        bytes32 week,
        address bettor,
        bytes calldata finalEntry
    ) public view returns (uint256 amount, bool refund) {
        WeekPool storage p = weekPools[week];
        if (p.poolClose == 0 || weekClaimed[week][bettor]) return (0, false);
        uint256 staked = weekStaked[week][bettor];
        if (staked == 0) return (0, false);
        (Outcome outcome, bytes32[] memory champions, bool badEntry) = _weekStanding(week, finalEntry);
        if (badEntry || outcome == Outcome.Pending) return (0, false);
        if (outcome == Outcome.FullRefund || p.sides < 2) return (staked, true);
        uint256 winnersStake;
        uint256 mine;
        for (uint256 i = 0; i < champions.length; i++) {
            winnersStake += weekBotStake[week][champions[i]];
            mine += weekStakes[week][bettor][champions[i]];
        }
        if (winnersStake == 0) return (staked * (BPS - FEE_BPS) / BPS, true);
        uint256 total = p.total;
        uint256 net = total - total * FEE_BPS / BPS;
        return (mine * net / winnersStake, false);
    }

    /// Claim the caller's winnings and refunds on a list of weeks, each with
    /// its bound final's entry (empty where there's none).
    function claimWeeks(bytes32[] calldata weeks_, bytes[] calldata finalEntries) external {
        _claimWeeks(msg.sender, weeks_, finalEntries);
    }

    /// The same claim, callable by anyone, only ever paying the bettor.
    function claimWeeksFor(
        address bettor,
        bytes32[] calldata weeks_,
        bytes[] calldata finalEntries
    ) external {
        _claimWeeks(bettor, weeks_, finalEntries);
    }

    function _claimWeeks(
        address bettor,
        bytes32[] calldata weeks_,
        bytes[] calldata finalEntries
    ) internal nonReentrant {
        require(weeks_.length == finalEntries.length, "lengths");
        uint256 total;
        for (uint256 i = 0; i < weeks_.length; i++) {
            bytes32 week = weeks_[i];
            if (weekClaimed[week][bettor]) {
                emit Skipped(week, bettor, "already claimed");
                continue;
            }
            if (weekPools[week].poolClose == 0) {
                emit Skipped(week, bettor, "no pool");
                continue;
            }
            (Outcome outcome,, bool badEntry) = _weekStanding(week, finalEntries[i]);
            if (badEntry) {
                emit Skipped(week, bettor, "not the bound final's entry");
                continue;
            }
            if (outcome == Outcome.Pending) {
                emit Skipped(week, bettor, "no result yet");
                continue;
            }
            (uint256 amount, bool refund) = weekClaimable(week, bettor, finalEntries[i]);
            weekClaimed[week][bettor] = true;
            if (amount == 0) {
                emit Skipped(week, bettor, "nothing to pay");
                continue;
            }
            total += amount;
            emit WeekClaimed(week, bettor, amount, refund);
        }
        if (total > 0) require(TOKEN.transfer(bettor, total), "transfer");
    }

    /// The fee on a settled two-sided week pool, and its builder share.
    function weekFee(
        bytes32 week,
        bytes calldata finalEntry
    ) public view returns (uint256 total, uint256 builderShare, bytes32[] memory champions) {
        WeekPool storage p = weekPools[week];
        if (p.poolClose == 0 || p.feeClaimed) return (0, 0, champions);
        Outcome outcome;
        bool badEntry;
        (outcome, champions, badEntry) = _weekStanding(week, finalEntry);
        if (badEntry || outcome != Outcome.Settled || p.sides < 2) return (0, 0, new bytes32[](0));
        return (uint256(p.total) * FEE_BPS / BPS, uint256(p.total) * BUILDER_BPS / BPS, champions);
    }

    /// Claim the fee on many weeks, by anyone. The builder point goes to each
    /// champion's payout address as the field froze it, proven by its leaf
    /// (split equally among co-champions; the zero address's share goes to
    /// the treasury), the rest to the current treasury. Each transfer is
    /// isolated, as for sessions.
    function claimWeekFees(WeekFeeItem[] calldata items) external nonReentrant {
        uint256 toTreasury = treasuryOwed;
        treasuryOwed = 0;
        for (uint256 i = 0; i < items.length; i++) {
            WeekFeeItem calldata item = items[i];
            (uint256 total, uint256 builderShare, bytes32[] memory champions) =
                weekFee(item.week, item.finalEntry);
            if (total == 0) {
                emit Skipped(item.week, address(0), "no fee");
                continue;
            }
            if (!_championsProven(item, champions)) {
                emit Skipped(item.week, address(0), "champions don't match the field");
                continue;
            }
            weekPools[item.week].feeClaimed = true;
            uint256 each = builderShare / champions.length;
            uint256 paidToBuilders;
            for (uint256 j = 0; j < champions.length; j++) {
                address builder = item.champions[j].payout;
                if (builder == address(0)) continue;
                paidToBuilders += each;
                bool ok = _tryTransfer(builder, each);
                if (!ok) builderOwed[builder] += each;
                emit BuilderPaid(item.week, builder, each, ok);
            }
            toTreasury += total - paidToBuilders;
            emit WeekFeeClaimed(item.week, total - paidToBuilders, paidToBuilders);
        }
        _payTreasury(toTreasury);
    }

    function _championsProven(
        WeekFeeItem calldata item,
        bytes32[] memory champions
    ) internal view returns (bool) {
        if (item.champions.length != champions.length || item.proofs.length != champions.length) {
            return false;
        }
        bytes32 root = weekPools[item.week].fieldRoot;
        for (uint256 j = 0; j < champions.length; j++) {
            if (item.champions[j].digest != champions[j]) return false;
            if (!Merkle.verify(item.proofs[j], root, leafHash(item.champions[j]))) return false;
        }
        return true;
    }
}
