// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/// Checks an account wallet's ERC-1271 signature, deployed or not
/// (MONEY.md#stolen-sign-ins: "including from a wallet not yet deployed").
///
/// Never deployed: the api runs its creation code inside `eth_call`, with no
/// `to`. If the wallet has no code yet, the constructor first deploys it from
/// its setup data through the factory, inside that call only, then asks the
/// wallet. It returns one byte, 1 for a valid signature. The same idea as
/// ERC-6492's off-chain validator, cut to what account wallets need.
contract WalletSignatureCheck {
    bytes4 private constant MAGIC = 0x1626ba7e;

    constructor(
        address wallet,
        bytes32 hash,
        bytes memory signature,
        address factory,
        bytes memory factoryData
    ) {
        if (wallet.code.length == 0 && factory != address(0)) {
            // A failed deploy leaves no code, and the check below fails.
            (bool deployed,) = factory.call(factoryData);
            deployed;
        }
        bool valid;
        if (wallet.code.length != 0) {
            (bool ok, bytes memory ret) = wallet.staticcall(abi.encodeWithSelector(MAGIC, hash, signature));
            valid = ok && ret.length >= 32 && bytes4(abi.decode(ret, (bytes32))) == MAGIC;
        }
        assembly {
            mstore(0, valid)
            return(31, 1)
        }
    }
}
