// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReceiptClaim, ReceiptClaimLib} from "risc0/IRiscZeroVerifier.sol";

import {Results} from "../src/Results.sol";
import {Base} from "./helpers/Base.sol";

/// SubmitKey approval from the zk journal (TRUST.md#submitkey-lifecycle,
/// TRUST.md#minting-a-bundlekey).
contract KeyApprovalTest is Base {
    using ReceiptClaimLib for ReceiptClaim;

    function expectApprovalRefused(Results.Journal memory j, string memory reason) internal {
        bytes memory b = abi.encode(j);
        bytes memory s = seal(b);
        vm.prank(j.submitKey);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, reason));
        results.approveKey(verifierKey, s, b);
    }

    function test_a_key_is_approved_with_its_image_root_verifier_and_bundle_key_recorded() public {
        approve(enclave2, BUNDLE_KEY);
        (
            uint64 approvedAt,
            uint64 instruction,
            bytes32 image,
            bytes32 root,
            bytes32 verifier,
            bytes32 bundleKey
        ) = results.keys(enclave2);
        assertEq(approvedAt, vm.getBlockTimestamp());
        assertEq(instruction, 1);
        assertEq(image, IMAGE);
        assertEq(root, ROOT);
        assertEq(verifier, verifierKey);
        assertEq(bundleKey, BUNDLE_KEY);
    }

    function test_approval_is_accepted_only_from_the_key_it_approves() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(j);
        bytes memory s = seal(b);
        vm.prank(makeAddr("someone-else"));
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "not the sender's key"));
        results.approveKey(verifierKey, s, b);
    }

    function test_a_bad_proof_is_refused() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(j);
        bytes memory other =
            seal(abi.encode(journal(enclave2, keccak256("x"), 1, uint64(vm.getBlockTimestamp()))));
        vm.prank(enclave2);
        vm.expectRevert();
        results.approveKey(verifierKey, other, b);
    }

    function test_an_unapproved_verifier_is_refused() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        bytes memory b = abi.encode(j);
        bytes memory s = seal(b);
        vm.prank(enclave2);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "verifier not approved"));
        results.approveKey(keccak256("nope"), s, b);
    }

    function test_another_project_is_refused() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.projectId = keccak256("someone-elses-project");
        expectApprovalRefused(j, "project");
    }

    function test_a_testnet_key_can_never_be_approved_on_prod() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.chainId = 84_532;
        expectApprovalRefused(j, "binding");
        j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.results = address(0xDEAD);
        expectApprovalRefused(j, "binding");
    }

    function test_an_unapproved_image_or_root_is_refused() public {
        Results.Journal memory j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.imageDigest = keccak256("rogue-image");
        expectApprovalRefused(j, "image");
        j = journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        j.rootHash = keccak256("fake-root");
        expectApprovalRefused(j, "root");
    }

    function test_a_proof_older_than_seven_days_is_refused() public {
        uint64 old = uint64(vm.getBlockTimestamp() - 7 days - 1);
        expectApprovalRefused(journal(enclave2, BUNDLE_KEY, 1, old), "issue time");
    }

    function test_a_key_is_approved_once_and_a_revoked_key_never_comes_back() public {
        Results.Journal memory j = journal(enclave, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp()));
        expectApprovalRefused(j, "key known");
        vm.prank(admin);
        results.revokeKey(enclave2);
        expectApprovalRefused(journal(enclave2, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp())), "key known");
    }

    function test_a_revoked_key_re_proved_through_a_new_verifier_is_refused() public {
        vm.prank(admin);
        results.revokeVerifier(verifierKey);
        // A new verifier, approved through the timelock.
        vm.prank(admin);
        results.proposeVerifier(address(mockVerifier), keccak256("guest-2"));
        skip(2 days);
        bytes32 v2 = results.verifierId(address(mockVerifier), keccak256("guest-2"));
        bytes memory b = abi.encode(journal(enclave, BUNDLE_KEY, 1, uint64(vm.getBlockTimestamp())));
        bytes memory s = abi.encodePacked(SELECTOR, _claim(keccak256("guest-2"), b));
        vm.prank(enclave);
        vm.expectRevert(abi.encodeWithSelector(Results.BadJournal.selector, "key known"));
        results.approveKey(v2, s, b);
    }

    function _claim(bytes32 guest, bytes memory b) internal pure returns (bytes32) {
        return ReceiptClaimLib.ok(guest, sha256(b)).digest();
    }

    function test_an_instruction_not_yet_in_effect_is_refused() public {
        vm.prank(admin);
        results.orderInstruction();
        expectApprovalRefused(
            journal(enclave2, keccak256("k2"), 2, uint64(vm.getBlockTimestamp())), "instruction not in effect"
        );
    }

    function test_the_first_key_for_an_instruction_fills_it_and_a_different_key_is_refused() public {
        // Genesis is filled by `enclave` with BUNDLE_KEY in setUp.
        (, bytes32 held) = results.instructions(1);
        assertEq(held, BUNDLE_KEY);
        expectApprovalRefused(
            journal(enclave2, keccak256("a second minter's key"), 1, uint64(vm.getBlockTimestamp())),
            "instruction held under another key"
        );
        approve(enclave2, BUNDLE_KEY);
        assertTrue(results.keyActive(enclave2), "a holder of the same key is approved");
    }

    function test_a_first_key_outside_the_minting_window_is_refused_and_a_fresh_token_lands() public {
        vm.prank(admin);
        results.orderInstruction();
        skip(2 days);
        uint64 issued = uint64(vm.getBlockTimestamp());
        skip(results.MINTING_WINDOW() + 1);
        expectApprovalRefused(journal(enclave2, keccak256("k2"), 2, issued), "minting window");
        // The minter takes a fresh token, keeps its key, and lands it.
        approveWith(journal(enclave2, keccak256("k2"), 2, uint64(vm.getBlockTimestamp())));
        (, bytes32 held) = results.instructions(2);
        assertEq(held, keccak256("k2"));
    }

    function test_a_later_holder_of_a_filled_instruction_needs_no_minting_window() public {
        uint64 issued = uint64(vm.getBlockTimestamp());
        skip(results.MINTING_WINDOW() + 1);
        approveWith(journal(enclave2, BUNDLE_KEY, 1, issued));
        assertTrue(results.keyActive(enclave2));
    }

    function test_a_withdrawn_instruction_never_takes_effect() public {
        vm.startPrank(admin);
        results.orderInstruction();
        results.withdrawInstruction(2);
        vm.stopPrank();
        skip(2 days);
        assertFalse(results.instructionInEffect(2));
    }
}
