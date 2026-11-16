// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title The hourly post's Merkle tree
/// @notice Leaves are domain-separated from inner nodes, so no inner node can
/// pass as an entry (TRUST.md#the-hourly-post). Pairs are hashed in sorted
/// order, so a proof is a list of siblings with no positions. An odd node at
/// the end of a level moves up unchanged.
///
/// - leaf  = keccak256(0x00 ‖ keccak256(entry))   (keccak256(entry) is the session id)
/// - inner = keccak256(0x01 ‖ min(a, b) ‖ max(a, b))
library Merkle {
    function leaf(bytes32 entryHash) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x00), entryHash));
    }

    function node(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b
            ? keccak256(abi.encodePacked(bytes1(0x01), a, b))
            : keccak256(abi.encodePacked(bytes1(0x01), b, a));
    }

    /// The root over the leaves of these entry hashes, in list order.
    function root(bytes32[] memory entryHashes) internal pure returns (bytes32) {
        uint256 n = entryHashes.length;
        if (n == 0) return bytes32(0);
        bytes32[] memory level = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            level[i] = leaf(entryHashes[i]);
        }
        while (n > 1) {
            uint256 next = (n + 1) / 2;
            for (uint256 i = 0; i < next; i++) {
                uint256 left = 2 * i;
                level[i] = left + 1 < n ? node(level[left], level[left + 1]) : level[left];
            }
            n = next;
        }
        return level[0];
    }

    function verify(
        bytes32[] calldata proof,
        bytes32 root_,
        bytes32 entryHash
    ) internal pure returns (bool) {
        bytes32 h = leaf(entryHash);
        for (uint256 i = 0; i < proof.length; i++) {
            h = node(h, proof[i]);
        }
        return h == root_;
    }
}
