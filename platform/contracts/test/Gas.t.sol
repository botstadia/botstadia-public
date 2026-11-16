// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../src/Results.sol";
import {Settlement} from "../src/Settlement.sol";
import {Base} from "./helpers/Base.sol";

/// Gas figures for BEFORE-PROD items 6 and 7, logged with -vv. Base's real
/// figures are confirmed on the testnet; these are Anvil's EVM costs, without
/// L1 data fees.
contract GasTest is Base {
    /// The keeper's post budget (`posts::MAX_POST_BYTES`), counted as it
    /// counts: each entry's padded bytes plus its offset and length words.
    uint256 internal constant MAX_POST_BYTES = 120 * 1024;
    /// The sequencer's limit on one encoded transaction (measured on Base
    /// Sepolia: "oversized data … limit 131072").
    uint256 internal constant TX_LIMIT = 131_072;

    function fullPost(uint256 entrants) internal {
        uint64 close = uint64(vm.getBlockTimestamp() + 6 hours);
        uint256 cost = ((sessionEntry(spec(entrants, 0), close).length + 31) / 32) * 32 + 64;
        uint256 n = MAX_POST_BYTES / cost;
        bytes[] memory entries = new bytes[](n);
        for (uint256 i = 0; i < n; i++) {
            entries[i] = sessionEntry(spec(entrants, bytes32(i)), close);
        }
        uint256 calldataBytes = abi.encodeCall(Results.post, (entries)).length;
        // A signed type-2 transaction adds well under 300 bytes to its calldata.
        assertLt(calldataBytes + 300, TX_LIMIT, "a full post fits one transaction");
        vm.prank(operator);
        uint256 g = gasleft();
        results.post(entries);
        uint256 used = g - gasleft();
        emit log_named_uint(string.concat("full post of ", vm.toString(n), " sessions, execution gas"), used);
        emit log_named_uint("  entrants a session", entrants);
        emit log_named_uint("  calldata bytes", calldataBytes);
        // Base's per-transaction gas cap (EIP-7825), with calldata's own gas.
        assertLt(used + 21_000 + calldataBytes * 16, 1 << 24, "under the per-transaction gas cap");
    }

    function test_gas_of_a_full_post_of_two_entrant_sessions() public {
        fullPost(2);
    }

    function test_gas_of_a_full_post_of_six_entrant_sessions() public {
        fullPost(6);
    }

    function test_gas_per_session_entry_in_full_batches() public {
        uint256 n = 40;
        Settlement.Spec[] memory specs = new Settlement.Spec[](n);
        for (uint256 i = 0; i < n; i++) {
            specs[i] = spec(6, bytes32(i));
        }
        Session[] memory s = postSessions(specs, uint64(vm.getBlockTimestamp() + 6 hours));
        vm.warp(s[0].close + results.ANCHOR_MARGIN());
        vm.roll(vm.getBlockNumber() + 1);
        bytes32 hash = blockhash(vm.getBlockNumber() - 1);
        uint64 number = uint64(vm.getBlockNumber() - 1);
        Results.AnchorItem[] memory anchors = new Results.AnchorItem[](n);
        for (uint256 i = 0; i < n; i++) {
            anchors[i] = anchorItem(s[i]);
        }
        vm.prank(enclave);
        uint256 g = gasleft();
        results.anchor(anchors);
        uint256 anchorGas = g - gasleft();
        emit log_named_uint("anchor batch of 40 (6 entrants each), execution gas", anchorGas);
        emit log_named_uint("  per anchor entry", anchorGas / n);
        emit log_named_uint("  calldata bytes", abi.encodeCall(Results.anchor, (anchors)).length);

        skip(results.RESULT_MIN_AGE());
        Results.ResultItem[] memory items = new Results.ResultItem[](n);
        uint8[] memory places = new uint8[](6);
        for (uint256 j = 0; j < 6; j++) {
            places[j] = uint8(j);
        }
        for (uint256 i = 0; i < n; i++) {
            items[i] = Results.ResultItem({
                session: s[i].id,
                anchor: hash,
                anchorBlock: number,
                specHash: settlement.specHash(s[i].spec),
                outcome: order(places),
                seed: keccak256("seed"),
                archiveHash: keccak256("archive"),
                signature: new bytes(65)
            });
        }
        vm.prank(enclave);
        g = gasleft();
        results.submit(items, new Results.RatingItem[](0));
        uint256 resultGas = g - gasleft();
        emit log_named_uint("result batch of 40, execution gas", resultGas);
        emit log_named_uint("  per result entry", resultGas / n);
        emit log_named_uint(
            "  calldata bytes", abi.encodeCall(Results.submit, (items, new Results.RatingItem[](0))).length
        );
    }

    function test_gas_of_a_pool_opening_first_bet_and_a_later_bet_at_six_entrants() public {
        address a = bettor("a");
        address b = bettor("b");
        Session memory s = newSession(6);
        Settlement.Opening memory o = opening(s);
        bytes32 first = s.spec.digests[0];
        vm.prank(a);
        uint256 g = gasleft();
        settlement.openAndBetFor(o, first, USDC, a);
        emit log_named_uint("pool-opening first bet, 6 entrants, gas", g - gasleft());
        bytes32 second = s.spec.digests[1];
        vm.prank(b);
        g = gasleft();
        settlement.betFor(s.id, second, USDC, b);
        emit log_named_uint("later bet gas", g - gasleft());
        closeAndAnchor(s);
        uint8[] memory places = new uint8[](6);
        for (uint256 j = 0; j < 6; j++) {
            places[j] = uint8(j);
        }
        resolve(s, order(places));
        bytes32[] memory list = one(s.id);
        vm.prank(a);
        g = gasleft();
        settlement.claim(list);
        emit log_named_uint("claim of one session gas", g - gasleft());
    }
}
