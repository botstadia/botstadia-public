// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Results} from "../Results.sol";

/// @title BotStadia results contract, devnet variant
/// @notice The results contract with four durations shortened, so the
/// devnet never waits on the calendar (OPERATIONS.md#the-devnet): the
/// timelock, the result deadline, a close's minimum after its post and a
/// result's minimum anchor age. The devnet's image waits on Base's `safe`
/// head, a minute or two behind, so a 15-minute minimum age would be the
/// whole wait. Everything else, the minting window and the proof age
/// included, is prod's. Deployed only to the devnet: the deploy script gives
/// the public testnet and prod `Results` itself.
contract DevnetResults is Results {
    constructor(
        address admin_,
        address operator_,
        bytes32 projectId,
        address[] memory predecessors,
        uint256[] memory cutoffs
    ) Results(admin_, operator_, projectId, predecessors, cutoffs) {}

    function TIMELOCK() public pure override returns (uint64) {
        return 10 minutes;
    }

    function RESULT_DEADLINE() public pure override returns (uint64) {
        return 2 hours;
    }

    function CLOSE_MIN_AFTER_POST() public pure override returns (uint64) {
        return 2 minutes;
    }

    function RESULT_MIN_AGE() public pure override returns (uint64) {
        return 2 minutes;
    }
}
