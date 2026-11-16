// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC1271} from "openzeppelin/interfaces/IERC1271.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";

/// A minimal contract wallet that approves signatures by its owner key
/// through ERC-1271, as a Safe or any contract bettor would.
contract ContractWallet is IERC1271 {
    address public immutable OWNER;
    bool public burnGas;

    constructor(address owner) {
        OWNER = owner;
    }

    function setBurnGas(bool on) external {
        burnGas = on;
    }

    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4) {
        if (burnGas) {
            uint256 x;
            while (gasleft() > 1000) x++;
        }
        (address signer,,) = ECDSA.tryRecover(hash, signature);
        return signer == OWNER ? IERC1271.isValidSignature.selector : bytes4(0xffffffff);
    }

    function call(address target, bytes calldata data) external returns (bytes memory) {
        require(msg.sender == OWNER, "owner");
        (bool ok, bytes memory ret) = target.call(data);
        require(ok, "call failed");
        return ret;
    }
}
