// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

import {Deploy} from "../script/Deploy.s.sol";
import {Results} from "../src/Results.sol";

/// Exposes the deploy script's choices.
contract DeployHarness is Deploy {
    function results(string memory target) external returns (Results) {
        return newResults(
            target, address(1), address(2), keccak256("botstadia"), new address[](0), new uint256[](0)
        );
    }

    function chain(string memory target, uint256 chainId) external pure {
        checkChain(target, chainId);
    }
}

/// The public testnet runs prod's contracts exactly; only the devnet shortens
/// four durations, and nothing about that reaches the testnet or prod
/// (OPERATIONS.md#targets).
contract TargetsTest is Test {
    DeployHarness deploy = new DeployHarness();

    function plain() internal returns (Results) {
        return new Results(address(1), address(2), keccak256("botstadia"), new address[](0), new uint256[](0));
    }

    function test_ProdTheTestnetAndLocalDeployResultsItselfWithProdDurations() public {
        string[3] memory targets = ["prod", "testnet", "local"];
        for (uint256 i; i < targets.length; i++) {
            Results r = deploy.results(targets[i]);
            // The very code a plain `Results` has: the variant never reaches
            // the testnet or prod.
            assertEq(address(r).codehash, address(plain()).codehash);
            assertEq(r.TIMELOCK(), 2 days);
            assertEq(r.RESULT_DEADLINE(), 24 hours);
            assertEq(r.CLOSE_MIN_AFTER_POST(), 1 hours);
            assertEq(r.RESULT_MIN_AGE(), 15 minutes);
        }
    }

    function test_TheDevnetShortensOnlyItsFourDurations() public {
        Results t = deploy.results("devnet");
        Results p = plain();
        assertTrue(address(t).codehash != address(p).codehash);
        assertEq(t.TIMELOCK(), 10 minutes);
        assertEq(t.RESULT_DEADLINE(), 2 hours);
        assertEq(t.CLOSE_MIN_AFTER_POST(), 2 minutes);
        assertEq(t.RESULT_MIN_AGE(), 2 minutes);
        // What the devnet measures stays prod's.
        assertEq(t.MINTING_WINDOW(), p.MINTING_WINDOW());
        assertEq(t.PROOF_MAX_AGE(), p.PROOF_MAX_AGE());
        assertEq(t.CLOSE_MAX_AFTER_POST(), p.CLOSE_MAX_AFTER_POST());
        assertEq(t.ANCHOR_MARGIN(), p.ANCHOR_MARGIN());
    }

    function test_EachTargetDeploysOnlyToItsOwnChain() public {
        deploy.chain("prod", 8453);
        deploy.chain("testnet", 84_532);
        deploy.chain("devnet", 84_532);
        deploy.chain("local", 31_337);
        vm.expectRevert(bytes("prod deploys only to Base mainnet"));
        deploy.chain("prod", 84_532);
        vm.expectRevert(bytes("testnet deploys only to Base Sepolia"));
        deploy.chain("testnet", 8453);
        vm.expectRevert(bytes("devnet deploys only to Base Sepolia"));
        deploy.chain("devnet", 8453);
        vm.expectRevert(bytes("CHAIN_TARGET is local, devnet, testnet or prod"));
        deploy.chain("staging", 8453);
    }
}
