// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// A real RISC Zero Groth16 proof (risc0-ethereum v3.0.1's circuit) of an
/// earlier attestation guest over a genuine Confidential Space token. Its
/// journal isn't ours, so it can't approve a key here: it exists to prove
/// that the RiscZeroGroth16Verifier we deploy verifies a real seal, and to
/// measure that verification's gas (BEFORE-PROD.md item 7).
///
/// That guest committed sha256 of a 188-byte canonical journal, rebuilt here
/// from the 160-byte ABI journal and the 64-byte tail of the 324-byte seal.
library RealGroth16Fixture {
    bytes32 internal constant IMAGE_ID = 0x8380b1a222ddbf918b25839f79b6e4aa9b7bab73e307cf21f8938ea1dacd6cef;
    uint256 internal constant GROTH16_SEAL_LENGTH = 260;
    bytes internal constant SEAL =
        hex"73c457ba25e181977e8c883f5b634a679787993ca01c67e98b638b8ed4e8b142f8ae16590f2abf3148a8c3cfa0a15c808513018f711687fa42a25232b4a5bf5650851852041f795b268d641e35fbd8a1e503af5abbb573d858a9534150a13f872a59cdb3228b91bb4c0f3113553524669b31f3537ddf31f5b54cc8b5e139d7cea1361f760348b1d44225a319766775b2d605558829c17c3f158098ff5d670d42c431ebe318face8a4e1bef4655e5f42c3d6e9aa1963103397a5a2a02540cf9f47d861461093bba1bead563fe79a96182cf976be6eb6762f0c1294488075049fc2feb9b0411c9ce1ec5898490f4d18b7037154d01c778ad99b2b203954f417caa5c6dd0b300000000000000000000000000000000000000000000000000000000000000008c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
    bytes internal constant JOURNAL =
        hex"b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f000000000000000000000000e890cd248bed3547dc084cfb22c62da62f410539f159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164000000000000000000000000000000000000000000000000000000006ac56093";

    function groth16Seal() internal pure returns (bytes memory out) {
        bytes memory seal = SEAL;
        out = new bytes(GROTH16_SEAL_LENGTH);
        for (uint256 i = 0; i < GROTH16_SEAL_LENGTH; i++) {
            out[i] = seal[i];
        }
    }

    function journalDigest() internal pure returns (bytes32) {
        bytes memory j = JOURNAL;
        bytes memory seal = SEAL;
        bytes memory canonical = new bytes(188);
        uint256 at;
        for (uint256 i = 0; i < 64; i++) {
            canonical[at++] = j[i];
        } // image digest, project id
        for (uint256 i = 96; i < 128; i++) {
            canonical[at++] = j[i];
        } // vaults root
        for (uint256 i = 152; i < 160; i++) {
            canonical[at++] = j[i];
        } // issued at
        for (uint256 i = 76; i < 96; i++) {
            canonical[at++] = j[i];
        } // EVM signer
        for (uint256 i = GROTH16_SEAL_LENGTH; i < seal.length; i++) {
            canonical[at++] = seal[i];
        }
        return sha256(canonical);
    }
}
