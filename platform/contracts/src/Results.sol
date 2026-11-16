// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IRiscZeroVerifier} from "risc0/IRiscZeroVerifier.sol";

import {Merkle} from "./Merkle.sol";

/// @title BotStadia results contract
/// @notice The public record of what happened (MONEY.md#contracts, TRUST.md).
/// It holds the approved SubmitKeys, images, GoogleRootCerts and verifiers,
/// new-key instructions, the hourly posts' roots, cancellations of sessions
/// and of weeks, each week's binding to its final, seed bots, every session's
/// anchor and bound key, every accepted result and each bot's first final
/// rating.
///
/// Immutable: no proxy, no upgrade. Logic never changes in place; trust
/// anchors are data the logic checks. Every duration is block time, never a
/// block count. Every admin gate maps to exactly one row of the admin powers
/// table (TRUST.md#admin-powers), quoted on its function.
contract Results {
    // ---------------------------------------------------------------- constants

    // Four durations are functions rather than constants, so the devnet's
    // variant (src/devnet/DevnetResults.sol) can shorten them; nothing else
    // differs, and these are prod's values, which the public testnet runs
    // too (OPERATIONS.md#targets).

    /// Every addition of a trust anchor, a new-key instruction and an admin
    /// transfer waits this long, in public.
    function TIMELOCK() public pure virtual returns (uint64) {
        return 2 days;
    }

    /// A session's result deadline is always its close time plus this.
    function RESULT_DEADLINE() public pure virtual returns (uint64) {
        return 24 hours;
    }

    /// An anchor's block time must be at least this far past close: more than
    /// one block step at Base's whole-second timestamps, so the previous block,
    /// whose hash is the anchor, came after close.
    uint64 public constant ANCHOR_MARGIN = 4 seconds;
    /// A result is accepted only once its anchor is this old by block time.

    function RESULT_MIN_AGE() public pure virtual returns (uint64) {
        return 15 minutes;
    }

    /// A first bet's close time must be at least this long after its post...

    function CLOSE_MIN_AFTER_POST() public pure virtual returns (uint64) {
        return 1 hours;
    }

    /// ...and no more than this.
    uint64 public constant CLOSE_MAX_AFTER_POST = 7 days;
    /// Approval accepts only proofs of tokens issued this recently.
    uint64 public constant PROOF_MAX_AGE = 7 days;
    /// The first key for a new-key instruction must come from a token issued
    /// within this window of its approval's block time.
    uint64 public constant MINTING_WINDOW = 4 hours;

    uint8 internal constant ENTRY_ARENA = 1;
    uint8 internal constant ENTRY_RATING = 2;
    uint8 internal constant ENTRY_SESSION = 3;
    uint8 internal constant ENTRY_FIELD = 4;
    uint8 internal constant ENTRY_STANDINGS = 5;
    /// A session entry's round for its week's final; league rounds count from 1.
    uint32 public constant ROUND_FINAL = 0;

    uint8 internal constant OUTCOME_ORDER = 0;
    uint8 internal constant OUTCOME_VOID = 1;

    // ------------------------------------------------------------- immutables

    /// keccak256 of our GCP project id: only our project's enclaves can be
    /// approved (TRUST.md#the-tee-and-the-zk-proof).
    bytes32 public immutable PROJECT_ID;

    /// Earlier results contracts whose ratings and seed namings carry over, up
    /// to each one's cut-off block (TRUST.md#redeploying-a-results-contract).
    /// Fixed by the constructor; data the logic checks, not a trust anchor.
    address[] internal _predecessors;
    uint256[] internal _cutoffs;

    // ----------------------------------------------------------------- types

    /// A timelocked item: approved once `readyAt` has passed.
    struct Gate {
        uint64 readyAt; // 0: never proposed, or withdrawn
        bool retired;
        bool revoked;
    }

    struct Verifier {
        address verifier;
        bytes32 guestImageId;
    }

    struct Instruction {
        uint64 readyAt; // 0: withdrawn
        bytes32 bundleKey; // the first key approved under it; 0 until filled
    }

    /// The zk guest's journal, ABI-encoded (TRUST.md#the-tee-and-the-zk-proof).
    struct Journal {
        bytes32 imageDigest; // the container image digest
        bytes32 projectId; // keccak256 of the GCP project id
        address submitKey; // the enclave's SubmitKey
        bytes32 bundleKey; // the BundleKey's public half (X25519)
        uint256 chainId; // the chain binding: this chain...
        address results; // ...and this contract
        uint64 instruction; // the new-key instruction its BundleKey belongs to
        uint64 issuedAt; // the token's issue time
        bytes32 rootHash; // the GoogleRootCert it chained to
    }

    struct Key {
        uint64 approvedAt;
        uint64 instruction;
        bytes32 image;
        bytes32 root;
        bytes32 verifierId;
        bytes32 bundleKey;
    }

    struct Post {
        bytes32 root;
        uint64 time;
    }

    /// A session's anchor, packed into one slot. The anchor hash, its block
    /// number and the spec hash live in `anchorCommit` as one hash, to keep
    /// an anchor at two storage slots (MONEY.md#the-gas-rule); the
    /// `Anchored` event carries them in full.
    struct Anchor {
        address key; // the SubmitKey the session is bound to
        uint40 time; // the anchoring block's time
        uint40 close;
        uint8 entrants;
    }

    struct Result {
        bool accepted;
        bool voided;
        uint16 voidReason;
        /// Bit i set: the spec's entrant i finished first (alone or tied).
        uint224 firstMask;
        bytes32 archiveHash;
    }

    struct Rating {
        bool set;
        bytes32 ladder;
        int64 mu; // fixed precision, 1e6
        int64 sigma;
        uint32 ratingVersion;
    }

    /// An anchor batch's entry. A week's final also carries its week's field
    /// entry, so the anchor can bind the week to it
    /// (TRUST.md#the-finals-gate); a league session leaves it empty.
    struct AnchorItem {
        uint64 post;
        bytes entry;
        bytes32[] proof;
        bytes fieldEntry;
    }

    /// A week's one field entry: the first posted for it. Any later field
    /// entry naming the same week is ignored by everything that reads a week.
    struct WeekField {
        bytes32 entryHash;
        uint64 post;
    }

    /// A week's field entry, decoded (TRUST.md#the-hourly-post).
    struct FieldEntry {
        bytes32 week;
        bytes32 ladder;
        bytes32 arena;
        uint32 budgetVersion;
        uint32 ratingVersion;
        uint32 standingsVersion;
        bytes32 fieldRoot;
        uint64 poolClose;
        uint64 finalClose;
        /// A final and a week-winner pool: a field of at least twice the
        /// arena's minimum entrants, and any raise, at the freeze.
        bool championship;
        /// The league rounds the week plays, which the enclave's final's gate
        /// enforces (TRUST.md#the-finals-gate).
        uint32 rounds;
    }

    struct ResultItem {
        bytes32 session;
        bytes32 anchor;
        uint64 anchorBlock;
        bytes32 specHash;
        bytes outcome;
        bytes32 seed;
        bytes32 archiveHash;
        bytes signature;
    }

    struct RatingItem {
        bytes32 bot; // the bot's bundle digest
        bytes32 ladder;
        int64 mu;
        int64 sigma;
        uint32 ratingVersion;
        bytes32 arenaDigest;
        uint32 budgetVersion;
        bytes32 opponentListHash;
        bytes signature;
    }

    // ----------------------------------------------------------------- state

    address public admin;
    address public operator;
    bool public renounced;
    uint64 public renouncedAt;
    address public pendingAdmin;
    uint64 public pendingAdminReadyAt;

    mapping(bytes32 => Gate) public images;
    mapping(bytes32 => Gate) public roots;
    mapping(bytes32 => Gate) public verifierGates;
    mapping(bytes32 => Verifier) public verifiers;
    mapping(uint64 => Instruction) public instructions;
    uint64 public instructionCount;

    mapping(address => Key) public keys;
    mapping(address => bool) public revokedKeys;

    mapping(uint64 => Post) public posts;
    uint64 public postCount;
    bool public postsClosed;

    mapping(bytes32 => bool) public cancelled;
    /// Weeks the admin called off: no final can be bound to them.
    mapping(bytes32 => bool) public weekCancelled;
    /// Each week's final, bound at the final's anchor; zero until then.
    mapping(bytes32 => bytes32) public weekFinal;
    /// Each week's field entry, fixed by the first post naming it.
    mapping(bytes32 => WeekField) public weekFields;
    mapping(bytes32 => Anchor) public anchors;
    /// keccak256(abi.encode(anchor hash, its block number, spec hash)).
    mapping(bytes32 => bytes32) public anchorCommit;
    mapping(bytes32 => Result) public results;

    mapping(bytes32 => Rating) public ratings;
    mapping(bytes32 => bool) public seedBots;
    mapping(bytes32 => bytes32) public seedLadders;
    /// Each ladder's count of rated and seed bots, each bot counted once.
    mapping(bytes32 => uint256) public ladderCount;
    mapping(bytes32 => bool) internal _counted;

    // ---------------------------------------------------------------- events

    event Proposed(bytes32 indexed kind, bytes32 indexed id, uint64 readyAt);
    event Withdrawn(bytes32 indexed kind, bytes32 indexed id);
    event Revoked(bytes32 indexed kind, bytes32 indexed id);
    event Retired(bytes32 indexed kind, bytes32 indexed id);
    event KeyApproved(
        address indexed key,
        bytes32 indexed image,
        bytes32 root,
        bytes32 verifierId,
        uint64 instruction,
        bytes32 bundleKey
    );
    event KeyRevoked(address indexed key);
    event OperatorChanged(address operator);
    event AdminTransferred(address admin);
    event Renounced();
    event PostsClosed();
    event Posted(uint64 indexed post, bytes32 root, bytes[] entries);
    event Cancelled(bytes32 indexed session);
    event WeekCancelled(bytes32 indexed week);
    event WeekBound(bytes32 indexed week, bytes32 indexed session);
    event WeekFieldPosted(bytes32 indexed week, uint64 post, bytes32 entryHash);
    event Anchored(
        bytes32 indexed session, address indexed key, bytes32 anchor, uint64 blockNumber, bytes32 specHash
    );
    event ResultAccepted(
        bytes32 indexed session,
        address indexed key,
        bytes outcome,
        bytes32 seed,
        bytes32 archiveHash,
        bytes signature
    );
    event RatingAccepted(
        bytes32 indexed bot,
        bytes32 indexed ladder,
        address indexed key,
        int64 mu,
        int64 sigma,
        uint32 ratingVersion,
        bytes32 arenaDigest,
        uint32 budgetVersion,
        bytes32 opponentListHash,
        bytes signature
    );
    event SeedBotNamed(bytes32 indexed bot, bytes32 indexed ladder);
    /// An entry skipped in a batch or list, with why. The rest go through.
    event Rejected(bytes32 indexed id, string reason);

    // ---------------------------------------------------------------- errors

    error NotAdmin();
    error NotOperator();
    error Timelocked();
    error NotPending();
    error AlreadyKnown();
    error RevokedForGood();
    error NotApproved();
    error Closed();
    error NoEntries();
    error KeyNotActive();
    error BadJournal(string reason);

    // ------------------------------------------------------------ kinds (ids)

    bytes32 internal constant KIND_IMAGE = "image";
    bytes32 internal constant KIND_ROOT = "root";
    bytes32 internal constant KIND_VERIFIER = "verifier";
    bytes32 internal constant KIND_INSTRUCTION = "instruction";
    bytes32 internal constant KIND_ADMIN = "admin";

    constructor(
        address admin_,
        address operator_,
        bytes32 projectId,
        address[] memory predecessors_,
        uint256[] memory cutoffs_
    ) {
        require(admin_ != address(0) && operator_ != address(0), "zero address");
        require(predecessors_.length == cutoffs_.length, "cut-offs");
        admin = admin_;
        operator = operator_;
        PROJECT_ID = projectId;
        _predecessors = predecessors_;
        _cutoffs = cutoffs_;
    }

    modifier onlyAdmin() {
        if (msg.sender != admin || renounced) revert NotAdmin();
        _;
    }

    // ------------------------------------------------------- timelocked gates

    /// Is a timelocked item in effect? A renounce cancels every action still
    /// pending at that moment.
    function _ready(uint64 readyAt) internal view returns (bool) {
        if (readyAt == 0 || block.timestamp < readyAt) return false;
        return !renounced || readyAt <= renouncedAt;
    }

    function _approved(Gate storage gate) internal view returns (bool) {
        return _ready(gate.readyAt) && !gate.revoked && !gate.retired;
    }

    function _propose(Gate storage gate, bytes32 kind, bytes32 id) internal {
        if (gate.revoked) revert RevokedForGood();
        if (gate.readyAt != 0) revert AlreadyKnown();
        uint64 readyAt = uint64(block.timestamp) + TIMELOCK();
        gate.readyAt = readyAt;
        emit Proposed(kind, id, readyAt);
    }

    function _withdraw(Gate storage gate, bytes32 kind, bytes32 id) internal {
        if (gate.readyAt == 0 || block.timestamp >= gate.readyAt) revert NotPending();
        gate.readyAt = 0;
        emit Withdrawn(kind, id);
    }

    function _revoke(Gate storage gate, bytes32 kind, bytes32 id) internal {
        gate.revoked = true;
        emit Revoked(kind, id);
    }

    function _retire(Gate storage gate, bytes32 kind, bytes32 id) internal {
        if (!_ready(gate.readyAt) || gate.revoked) revert NotApproved();
        gate.retired = true;
        emit Retired(kind, id);
    }

    /// Admin powers: "Approve an image digest", 2 days.
    function proposeImage(bytes32 digest) external onlyAdmin {
        _propose(images[digest], KIND_IMAGE, digest);
    }

    /// Admin powers: "Add a GoogleRootCert", 2 days.
    function proposeRoot(bytes32 rootHash) external onlyAdmin {
        _propose(roots[rootHash], KIND_ROOT, rootHash);
    }

    function verifierId(address verifier, bytes32 guestImageId) public pure returns (bytes32) {
        return keccak256(abi.encode(verifier, guestImageId));
    }

    /// Admin powers: "Approve a verifier", 2 days. A verifier is a verifier
    /// contract together with the guest image id it checks.
    function proposeVerifier(address verifier, bytes32 guestImageId) external onlyAdmin {
        bytes32 id = verifierId(verifier, guestImageId);
        _propose(verifierGates[id], KIND_VERIFIER, id);
        verifiers[id] = Verifier(verifier, guestImageId);
    }

    /// Admin powers: "Order a new BundleKey (genesis, after the key is lost,
    /// or after a leak)", 2 days. Instructions are numbered from 1; the first
    /// is genesis.
    function orderInstruction() external onlyAdmin returns (uint64 number) {
        number = ++instructionCount;
        uint64 readyAt = uint64(block.timestamp) + TIMELOCK();
        instructions[number].readyAt = readyAt;
        emit Proposed(KIND_INSTRUCTION, bytes32(uint256(number)), readyAt);
    }

    /// Admin powers: "Transfer the admin key", 2 days.
    function proposeAdmin(address next) external onlyAdmin {
        require(next != address(0), "zero address");
        pendingAdmin = next;
        pendingAdminReadyAt = uint64(block.timestamp) + TIMELOCK();
        emit Proposed(KIND_ADMIN, bytes32(uint256(uint160(next))), pendingAdminReadyAt);
    }

    /// Completes a transfer whose 2 days have passed. Anyone may call it.
    function acceptAdmin() external {
        if (pendingAdmin == address(0) || !_ready(pendingAdminReadyAt)) revert Timelocked();
        admin = pendingAdmin;
        pendingAdmin = address(0);
        pendingAdminReadyAt = 0;
        emit AdminTransferred(admin);
    }

    /// Admin powers: "Withdraw a pending timelocked action", instant.
    function withdrawImage(bytes32 digest) external onlyAdmin {
        _withdraw(images[digest], KIND_IMAGE, digest);
    }

    function withdrawRoot(bytes32 rootHash) external onlyAdmin {
        _withdraw(roots[rootHash], KIND_ROOT, rootHash);
    }

    function withdrawVerifier(bytes32 id) external onlyAdmin {
        _withdraw(verifierGates[id], KIND_VERIFIER, id);
    }

    function withdrawInstruction(uint64 number) external onlyAdmin {
        Instruction storage instruction = instructions[number];
        if (instruction.readyAt == 0 || block.timestamp >= instruction.readyAt) revert NotPending();
        instruction.readyAt = 0;
        emit Withdrawn(KIND_INSTRUCTION, bytes32(uint256(number)));
    }

    function withdrawAdmin() external onlyAdmin {
        if (pendingAdmin == address(0) || block.timestamp >= pendingAdminReadyAt) revert NotPending();
        emit Withdrawn(KIND_ADMIN, bytes32(uint256(uint160(pendingAdmin))));
        pendingAdmin = address(0);
        pendingAdminReadyAt = 0;
    }

    /// Admin powers: "Revoke a SubmitKey, an image, a GoogleRootCert or a
    /// verifier", instant. Also bans one proposed but not yet valid, for good.
    /// Revoking an image, root or verifier revokes every key it approved.
    function revokeImage(bytes32 digest) external onlyAdmin {
        _revoke(images[digest], KIND_IMAGE, digest);
    }

    function revokeRoot(bytes32 rootHash) external onlyAdmin {
        _revoke(roots[rootHash], KIND_ROOT, rootHash);
    }

    function revokeVerifier(bytes32 id) external onlyAdmin {
        _revoke(verifierGates[id], KIND_VERIFIER, id);
    }

    function revokeKey(address key) external onlyAdmin {
        revokedKeys[key] = true;
        emit KeyRevoked(key);
    }

    /// Admin powers: "Retire an image, a GoogleRootCert or a verifier",
    /// instant. It approves no new key; keys it approved stand.
    function retireImage(bytes32 digest) external onlyAdmin {
        _retire(images[digest], KIND_IMAGE, digest);
    }

    function retireRoot(bytes32 rootHash) external onlyAdmin {
        _retire(roots[rootHash], KIND_ROOT, rootHash);
    }

    function retireVerifier(bytes32 id) external onlyAdmin {
        _retire(verifierGates[id], KIND_VERIFIER, id);
    }

    /// Admin powers: "Replace the operator address", instant.
    function setOperator(address next) external onlyAdmin {
        require(next != address(0), "zero address");
        operator = next;
        emit OperatorChanged(next);
    }

    /// Admin powers: "Close posts on a results contract", instant, permanent.
    function closePosts() external onlyAdmin {
        postsClosed = true;
        emit PostsClosed();
    }

    /// Admin powers: "Renounce the admin key", instant, permanent. Every gate
    /// freezes and every pending timelocked action is cancelled.
    function renounce() external onlyAdmin {
        renounced = true;
        renouncedAt = uint64(block.timestamp);
        admin = address(0);
        pendingAdmin = address(0);
        pendingAdminReadyAt = 0;
        emit Renounced();
    }

    /// Admin powers: "Name seed bots (a list)", instant. A later naming
    /// doesn't reset a bot; a rated bot keeps its rating; each bot counts
    /// once on its ladder.
    function nameSeedBots(bytes32[] calldata bots, bytes32[] calldata ladders) external onlyAdmin {
        require(bots.length == ladders.length, "lengths");
        for (uint256 i = 0; i < bots.length; i++) {
            bytes32 bot = bots[i];
            if (seedBots[bot]) {
                emit Rejected(bot, "already a seed bot");
                continue;
            }
            seedBots[bot] = true;
            seedLadders[bot] = ladders[i];
            _count(bot, ratings[bot].set ? ratings[bot].ladder : ladders[i]);
            emit SeedBotNamed(bot, ladders[i]);
        }
    }

    /// Admin powers: "Cancel sessions (one or a list)", before its anchor
    /// only, per session. Full refund, no fee. An anchored session is skipped.
    function cancel(bytes32[] calldata sessions) external onlyAdmin {
        for (uint256 i = 0; i < sessions.length; i++) {
            bytes32 id = sessions[i];
            if (anchors[id].key != address(0)) {
                emit Rejected(id, "already anchored");
                continue;
            }
            if (cancelled[id]) {
                emit Rejected(id, "already cancelled");
                continue;
            }
            cancelled[id] = true;
            emit Cancelled(id);
        }
    }

    /// Admin powers: "Call off a week", before its final's anchor only. Its
    /// week-winner pool refunds in full at once and no final can be bound to
    /// it; its sessions' own pools are untouched.
    function cancelWeeks(bytes32[] calldata weekIds) external onlyAdmin {
        for (uint256 i = 0; i < weekIds.length; i++) {
            bytes32 week = weekIds[i];
            if (weekFinal[week] != bytes32(0)) {
                emit Rejected(week, "final already bound");
                continue;
            }
            if (weekCancelled[week]) {
                emit Rejected(week, "already called off");
                continue;
            }
            weekCancelled[week] = true;
            emit WeekCancelled(week);
        }
    }

    // --------------------------------------------------------- key approval

    function instructionInEffect(uint64 number) public view returns (bool) {
        return number != 0 && number <= instructionCount && _ready(instructions[number].readyAt);
    }

    function imageApproved(bytes32 digest) external view returns (bool) {
        return _approved(images[digest]);
    }

    function rootApproved(bytes32 rootHash) external view returns (bool) {
        return _approved(roots[rootHash]);
    }

    function verifierApproved(bytes32 id) external view returns (bool) {
        return _approved(verifierGates[id]);
    }

    /// Approve the sender's SubmitKey from a zk proof of its attestation
    /// (TRUST.md#submitkey-lifecycle). Only the key itself can land it, so a
    /// dead machine's published proof can never be landed by anyone else.
    function approveKey(bytes32 verifier, bytes calldata seal, bytes calldata journal) external {
        Gate storage gate = verifierGates[verifier];
        if (!_approved(gate)) revert BadJournal("verifier not approved");
        Verifier memory v = verifiers[verifier];
        IRiscZeroVerifier(v.verifier).verify(seal, v.guestImageId, sha256(journal));

        Journal memory j = abi.decode(journal, (Journal));
        if (j.submitKey != msg.sender) revert BadJournal("not the sender's key");
        if (j.projectId != PROJECT_ID) revert BadJournal("project");
        if (j.chainId != block.chainid || j.results != address(this)) revert BadJournal("binding");
        if (!_approved(images[j.imageDigest])) revert BadJournal("image");
        if (!_approved(roots[j.rootHash])) revert BadJournal("root");
        if (j.issuedAt > block.timestamp || block.timestamp - j.issuedAt > PROOF_MAX_AGE) {
            revert BadJournal("issue time");
        }
        // A key is approved once: once revoked by any route it stays revoked,
        // and the revoked set is permanent.
        if (keys[msg.sender].approvedAt != 0 || revokedKeys[msg.sender]) revert BadJournal("key known");
        if (!instructionInEffect(j.instruction)) revert BadJournal("instruction not in effect");
        if (j.bundleKey == bytes32(0)) revert BadJournal("bundle key");
        Instruction storage instruction = instructions[j.instruction];
        if (instruction.bundleKey == bytes32(0)) {
            if (block.timestamp - j.issuedAt > MINTING_WINDOW) revert BadJournal("minting window");
            instruction.bundleKey = j.bundleKey;
        } else if (instruction.bundleKey != j.bundleKey) {
            revert BadJournal("instruction held under another key");
        }

        keys[msg.sender] = Key({
            approvedAt: uint64(block.timestamp),
            instruction: j.instruction,
            image: j.imageDigest,
            root: j.rootHash,
            verifierId: verifier,
            bundleKey: j.bundleKey
        });
        emit KeyApproved(msg.sender, j.imageDigest, j.rootHash, verifier, j.instruction, j.bundleKey);
    }

    /// A key counts as revoked when it, its image, its root or its verifier
    /// is in the revoked set (TRUST.md#retiring-and-revoking).
    function keyRevoked(address key) public view returns (bool) {
        Key storage k = keys[key];
        return revokedKeys[key] || images[k.image].revoked || roots[k.root].revoked
            || verifierGates[k.verifierId].revoked;
    }

    function keyActive(address key) public view returns (bool) {
        return keys[key].approvedAt != 0 && !keyRevoked(key);
    }

    // --------------------------------------------------------------- posts

    /// The hourly post (TRUST.md#the-hourly-post): the full list of entries.
    /// The contract hashes the list into the root itself and emits it, so no
    /// entry is under a root without being listed.
    function post(bytes[] calldata entries) external returns (uint64 number) {
        if (msg.sender != operator) revert NotOperator();
        if (postsClosed) revert Closed();
        if (entries.length == 0) revert NoEntries();
        number = ++postCount;
        bytes32[] memory hashes = new bytes32[](entries.length);
        for (uint256 i = 0; i < entries.length; i++) {
            hashes[i] = keccak256(entries[i]);
            // A week's field entry is the first posted for it: a later one
            // naming the same week can never be used (TRUST.md#the-hourly-post).
            if (isFieldEntry(entries[i])) {
                bytes32 week = bytes32(entries[i][32:64]);
                WeekField storage wf = weekFields[week];
                if (wf.entryHash == bytes32(0)) {
                    weekFields[week] = WeekField(hashes[i], number);
                    emit WeekFieldPosted(week, number, hashes[i]);
                } else if (wf.entryHash != hashes[i]) {
                    emit Rejected(week, "the week already has a field entry");
                }
            }
        }
        bytes32 root_ = Merkle.root(hashes);
        posts[number] = Post(root_, uint64(block.timestamp));
        emit Posted(number, root_, entries);
    }

    /// Is this entry in that post?
    function inPost(
        uint64 number,
        bytes calldata entry,
        bytes32[] calldata proof
    ) public view returns (bool) {
        bytes32 root_ = posts[number].root;
        return root_ != bytes32(0) && Merkle.verify(proof, root_, keccak256(entry));
    }

    /// A session entry: (kind 3, spec hash, ascending bundle digests, close,
    /// week, round), the round 0 for the week's final.
    function decodeSession(bytes calldata entry)
        public
        pure
        returns (bytes32 specHash, bytes32[] memory digests, uint64 close, bytes32 week, uint32 round)
    {
        return _decodeSession(entry);
    }

    function _decodeSession(bytes calldata entry)
        internal
        pure
        returns (bytes32 specHash, bytes32[] memory digests, uint64 close, bytes32 week, uint32 round)
    {
        uint8 kind;
        (kind, specHash, digests, close, week, round) =
            abi.decode(entry, (uint8, bytes32, bytes32[], uint64, bytes32, uint32));
        require(kind == ENTRY_SESSION, "not a session entry");
    }

    function isSessionEntry(bytes calldata entry) public pure returns (bool) {
        return entry.length >= 32 && uint256(bytes32(entry[0:32])) == ENTRY_SESSION;
    }

    /// A week's field entry: (kind 4, week, ladder, arena digest, budget,
    /// rating and standings versions, field root, pool close, final close,
    /// championship).
    function decodeField(bytes calldata entry) public pure returns (FieldEntry memory f) {
        uint8 kind;
        (
            kind,
            f.week,
            f.ladder,
            f.arena,
            f.budgetVersion,
            f.ratingVersion,
            f.standingsVersion,
            f.fieldRoot,
            f.poolClose,
            f.finalClose,
            f.championship,
            f.rounds
        ) = abi.decode(
            entry,
            (uint8, bytes32, bytes32, bytes32, uint32, uint32, uint32, bytes32, uint64, uint64, bool, uint32)
        );
        require(kind == ENTRY_FIELD, "not a field entry");
    }

    /// A well-formed field entry: its kind, its length, and every word
    /// canonical for its type, so decoding it can never revert. Anything
    /// else is no field entry, and is never recorded for a week.
    function isFieldEntry(bytes calldata entry) public pure returns (bool) {
        if (entry.length != 384 || uint256(bytes32(entry[0:32])) != ENTRY_FIELD) return false;
        for (uint256 w = 4; w <= 6; w++) {
            if (uint256(bytes32(entry[w * 32:w * 32 + 32])) > type(uint32).max) return false;
        }
        if (uint256(bytes32(entry[352:384])) > type(uint32).max) return false;
        for (uint256 w = 8; w <= 9; w++) {
            if (uint256(bytes32(entry[w * 32:w * 32 + 32])) > type(uint64).max) return false;
        }
        return uint256(bytes32(entry[320:352])) <= 1;
    }

    /// A week's field entry, if `entry` is the one posted for it.
    function weekFieldOf(
        bytes32 week,
        bytes calldata entry
    ) public view returns (bool ok, FieldEntry memory f) {
        WeekField storage wf = weekFields[week];
        if (wf.entryHash == bytes32(0) || keccak256(entry) != wf.entryHash) return (false, f);
        f = decodeField(entry);
        return (true, f);
    }

    // -------------------------------------------------------------- anchors

    /// One enclave's anchor batch (TRUST.md#every-session-one-flow). Each
    /// entry is checked on its own; a bad one is skipped with a rejection
    /// event and the rest are accepted.
    function anchor(AnchorItem[] calldata items) external {
        if (!keyActive(msg.sender)) revert KeyNotActive();
        for (uint256 i = 0; i < items.length; i++) {
            _anchorOne(items[i]);
        }
    }

    function _anchorOne(AnchorItem calldata item) internal {
        bytes32 id = keccak256(item.entry);
        if (!isSessionEntry(item.entry)) {
            emit Rejected(id, "not a session entry");
            return;
        }
        if (!inPost(item.post, item.entry, item.proof)) {
            emit Rejected(id, "not in that post");
            return;
        }
        (bytes32 specHash, bytes32[] memory digests, uint64 close, bytes32 week, uint32 round) =
            _decodeSession(item.entry);
        if (block.timestamp < uint256(close) + ANCHOR_MARGIN) {
            emit Rejected(id, "too soon after close");
            return;
        }
        if (block.timestamp >= uint256(close) + RESULT_DEADLINE()) {
            emit Rejected(id, "past the deadline");
            return;
        }
        if (cancelled[id]) {
            emit Rejected(id, "cancelled");
            return;
        }
        if (anchors[id].key != address(0)) {
            emit Rejected(id, "already anchored");
            return;
        }
        if (digests.length == 0 || digests.length > 224) {
            emit Rejected(id, "entrants");
            return;
        }
        if (round == ROUND_FINAL && !_bindable(id, item, week, close)) return;
        bytes32 hash = blockhash(block.number - 1);
        uint64 number = uint64(block.number - 1);
        anchors[id] = Anchor({
            key: msg.sender,
            time: uint40(block.timestamp),
            close: uint40(close),
            entrants: uint8(digests.length)
        });
        anchorCommit[id] = commitment(hash, number, specHash);
        emit Anchored(id, msg.sender, hash, number, specHash);
        if (round == ROUND_FINAL) {
            weekFinal[week] = id;
            emit WeekBound(week, id);
        }
    }

    /// A final binds its week at its anchor: it carries the week's one field
    /// entry, the week has a championship, the final was posted no earlier
    /// than the week-winner pool's close (so nobody knew its finalists while
    /// that pool was open), its close is the week's committed final close,
    /// the week isn't called off, and no final is bound to it yet.
    function _bindable(
        bytes32 id,
        AnchorItem calldata item,
        bytes32 week,
        uint64 close
    ) internal returns (bool) {
        if (!isFieldEntry(item.fieldEntry)) {
            emit Rejected(id, "not the week's field entry");
            return false;
        }
        (bool ok, FieldEntry memory f) = weekFieldOf(week, item.fieldEntry);
        if (!ok) {
            emit Rejected(id, "not the week's field entry");
            return false;
        }
        if (!f.championship) {
            emit Rejected(id, "the week has no championship");
            return false;
        }
        if (posts[item.post].time < f.poolClose) {
            emit Rejected(id, "posted before the week-winner pool closed");
            return false;
        }
        if (f.finalClose != close) {
            emit Rejected(id, "close isn't the committed final close");
            return false;
        }
        if (weekCancelled[week]) {
            emit Rejected(id, "week called off");
            return false;
        }
        if (weekFinal[week] != bytes32(0)) {
            emit Rejected(id, "week already has a final");
            return false;
        }
        return true;
    }

    function commitment(
        bytes32 anchorHash,
        uint64 blockNumber,
        bytes32 specHash
    ) public pure returns (bytes32) {
        return keccak256(abi.encode(anchorHash, blockNumber, specHash));
    }

    // -------------------------------------------------------------- results

    /// One enclave's result batch, with any final ratings riding in it
    /// (TRUST.md#submitting-results). Each entry is checked on its own; a bad
    /// one is skipped with a rejection event.
    function submit(ResultItem[] calldata items, RatingItem[] calldata ratingItems) external {
        if (!keyActive(msg.sender)) revert KeyNotActive();
        for (uint256 i = 0; i < items.length; i++) {
            _resultOne(items[i]);
        }
        for (uint256 i = 0; i < ratingItems.length; i++) {
            _ratingOne(ratingItems[i]);
        }
    }

    function _resultOne(ResultItem calldata item) internal {
        bytes32 id = item.session;
        Anchor storage a = anchors[id];
        if (a.key != msg.sender) {
            emit Rejected(id, "not the bound key");
            return;
        }
        if (results[id].accepted) {
            emit Rejected(id, "already resolved");
            return;
        }
        if (commitment(item.anchor, item.anchorBlock, item.specHash) != anchorCommit[id]) {
            emit Rejected(id, "anchor or spec mismatch");
            return;
        }
        if (block.timestamp < uint256(a.time) + RESULT_MIN_AGE()) {
            emit Rejected(id, "anchor too recent");
            return;
        }
        if (block.timestamp >= uint256(a.close) + RESULT_DEADLINE()) {
            emit Rejected(id, "past the deadline");
            return;
        }
        (bool ok, bool voided, uint16 reason, uint256 firstMask) = parseOutcome(item.outcome, a.entrants);
        if (!ok) {
            emit Rejected(id, "malformed outcome");
            return;
        }
        results[id] = Result({
            accepted: true,
            voided: voided,
            voidReason: reason,
            firstMask: uint224(firstMask),
            archiveHash: item.archiveHash
        });
        emit ResultAccepted(id, msg.sender, item.outcome, item.seed, item.archiveHash, item.signature);
    }

    /// An outcome is `abi.encode(uint8 kind, uint16 voidReason, uint8[] places)`:
    /// kind 0 a finishing order, where `places[i]` is the spec's entrant i's
    /// place (0 is first; ties share a place), or kind 1 a void with its
    /// reason code and no places.
    function parseOutcome(
        bytes calldata outcome,
        uint8 entrants
    ) public pure returns (bool ok, bool voided, uint16 reason, uint256 firstMask) {
        if (outcome.length < 96) return (false, false, 0, 0);
        (uint8 kind, uint16 voidReason, uint8[] memory places) = abi.decode(outcome, (uint8, uint16, uint8[]));
        if (kind == OUTCOME_VOID) {
            return (places.length == 0, true, voidReason, 0);
        }
        if (kind != OUTCOME_ORDER || places.length != entrants || voidReason != 0) {
            return (false, false, 0, 0);
        }
        for (uint256 i = 0; i < places.length; i++) {
            if (places[i] == 0) firstMask |= (uint256(1) << i);
        }
        return (firstMask != 0, false, 0, firstMask);
    }

    function _ratingOne(RatingItem calldata item) internal {
        bytes32 bot = item.bot;
        if (ratings[bot].set) {
            emit Rejected(bot, "already rated");
            return;
        }
        if (seedBots[bot]) {
            emit Rejected(bot, "a seed bot");
            return;
        }
        ratings[bot] = Rating({
            set: true,
            ladder: item.ladder,
            mu: item.mu,
            sigma: item.sigma,
            ratingVersion: item.ratingVersion
        });
        _count(bot, item.ladder);
        emit RatingAccepted(
            bot,
            item.ladder,
            msg.sender,
            item.mu,
            item.sigma,
            item.ratingVersion,
            item.arenaDigest,
            item.budgetVersion,
            item.opponentListHash,
            item.signature
        );
    }

    function _count(bytes32 bot, bytes32 ladder) internal {
        if (_counted[bot]) return;
        _counted[bot] = true;
        ladderCount[ladder] += 1;
    }

    // ---------------------------------------------------------------- reads

    function predecessors() external view returns (address[] memory, uint256[] memory) {
        return (_predecessors, _cutoffs);
    }

    /// What settlement needs about a week: called off, and its bound final.
    function weekState(bytes32 week) external view returns (bool isCancelled, bytes32 finalSession) {
        return (weekCancelled[week], weekFinal[week]);
    }

    function deadline(bytes32 session) external view returns (uint64) {
        Anchor storage a = anchors[session];
        return a.key == address(0) ? 0 : uint64(a.close) + RESULT_DEADLINE();
    }

    /// What settlement needs about a session, in one call. `close` is known
    /// here only once anchored; settlement keeps its own from the first bet.
    function sessionState(bytes32 session)
        external
        view
        returns (bool isCancelled, bool resolved, bool voided, uint256 firstMask, bool boundRevoked)
    {
        Result storage r = results[session];
        address key = anchors[session].key;
        return (cancelled[session], r.accepted, r.voided, r.firstMask, key != address(0) && keyRevoked(key));
    }
}
