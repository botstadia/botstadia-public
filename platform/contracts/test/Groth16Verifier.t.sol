// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ControlID, RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";

import {RealGroth16Fixture as F} from "./fixtures/RealGroth16Fixture.sol";

/// The Groth16 verifier we deploy and own (TRUST.md#the-tee-and-the-zk-proof),
/// against a real seal.
contract Groth16VerifierTest is Test {
    RiscZeroGroth16Verifier verifier;

    function setUp() public {
        verifier = new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
    }

    function test_a_real_groth16_seal_verifies() public {
        bytes memory seal = F.groth16Seal();
        bytes32 digest = F.journalDigest();
        uint256 g = gasleft();
        verifier.verify(seal, F.IMAGE_ID, digest);
        emit log_named_uint("Groth16 verify gas", g - gasleft());
    }

    function test_a_tampered_journal_or_image_id_is_refused() public {
        bytes memory seal = F.groth16Seal();
        bytes32 digest = F.journalDigest();
        vm.expectRevert();
        verifier.verify(seal, F.IMAGE_ID, keccak256("another journal"));
        vm.expectRevert();
        verifier.verify(seal, keccak256("another guest"), digest);
    }
}

/// Our own guest's real Groth16 proof (`a_real_groth16_proof_becomes_a_foundry_fixture`
/// in the attestation host crate writes the fixture), through the verifier we deploy.
/// Skips while the fixture hasn't been made.
contract OurGuestGroth16Test is Test {
    function test_our_guest_s_real_proof_verifies_against_sha256_of_its_journal() public {
        string memory path = "test/fixtures/guest-groth16.json";
        if (!vm.exists(path)) {
            vm.skip(true);
            return;
        }
        string memory json = vm.readFile(path);
        bytes32 imageId = vm.parseJsonBytes32(json, ".image_id");
        bytes memory journal = vm.parseJsonBytes(json, ".journal");
        bytes memory seal = vm.parseJsonBytes(json, ".seal");
        RiscZeroGroth16Verifier verifier =
            new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
        bytes32 digest = sha256(journal);
        verifier.verify(seal, imageId, digest);
        assertEq(journal.length, 288, "abi.encode(Results.Journal)");
    }
}
