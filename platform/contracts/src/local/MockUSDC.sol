// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";
import {ERC20Permit} from "openzeppelin/token/ERC20/extensions/ERC20Permit.sol";

/// @title A stand-in for native USDC, for the local target only
/// @notice Six decimals and permit, like USDC. Anyone may mint. A blocklist
/// stands in for Circle's power to freeze an address, so freezes can be
/// tested (MONEY.md#settlement).
contract MockUSDC is ERC20, ERC20Permit {
    mapping(address => bool) public frozen;

    constructor() ERC20("Mock USD Coin", "USDC") ERC20Permit("Mock USD Coin") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function setFrozen(address who, bool isFrozen) external {
        frozen[who] = isFrozen;
    }

    function _update(address from, address to, uint256 value) internal override {
        require(!frozen[from] && !frozen[to], "frozen");
        super._update(from, to, value);
    }
}
