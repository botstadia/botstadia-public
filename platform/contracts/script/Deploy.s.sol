// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script, console} from "forge-std/Script.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {ControlID, RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";
import {RiscZeroMockVerifier} from "risc0/test/RiscZeroMockVerifier.sol";

import {Results} from "../src/Results.sol";

import {Settlement} from "../src/Settlement.sol";
import {StandingOrders} from "../src/StandingOrders.sol";

import {DevnetResults} from "../src/devnet/DevnetResults.sol";
import {MockUSDC} from "../src/local/MockUSDC.sol";

/// Deploys the results, settlement and standing-orders contracts together,
/// each fixed to the one below it (TRUST.md#if-the-admin-key-is-stolen-or-lost),
/// plus a Groth16 verifier we own, unless `CHAIN_GROTH16_VERIFIER` names one
/// already deployed on the chain: a verifier is immutable and binds to no
/// results contract, so the testnet and the devnet, two bindings on Base
/// Sepolia, share one, as the image's constants for that chain do. The token, admin, operator, treasury,
/// project id, statement and predecessors come from the environment, which
/// the Taskfile fills from config.yml per target.
///
/// A results contract deploys with no image, root, verifier or new-key
/// instruction: the admin proposes each under the 2-day timelock afterwards
/// (on the admin site or with the hot admin key for the testnet and the
/// devnet, on the admin site for prod; `LocalSetup` for local).
///
/// On the local target only, it also deploys the mock USDC and RISC Zero's
/// mock verifier, and writes every address to deployments/local.json.
contract Deploy is Script {
    function run() external {
        string memory target = vm.envString("CHAIN_TARGET");
        bool local = keccak256(bytes(target)) == keccak256("local");
        checkChain(target, block.chainid);
        address admin = vm.envAddress("CHAIN_ADMIN");
        address operator = vm.envAddress("CHAIN_OPERATOR");
        address treasury = vm.envAddress("CHAIN_TREASURY");
        bytes32 projectId = keccak256(bytes(vm.envString("CHAIN_PROJECT_ID")));
        bytes32 statement = keccak256(bytes(vm.envString("CHAIN_STATEMENT")));
        address[] memory predecessors = vm.envOr("CHAIN_PREDECESSORS", ",", new address[](0));
        uint256[] memory cutoffs = vm.envOr("CHAIN_CUTOFFS", ",", new uint256[](0));

        vm.startBroadcast(vm.envUint("CHAIN_DEPLOYER_KEY"));
        IERC20 token = local ? IERC20(address(new MockUSDC())) : IERC20(vm.envAddress("CHAIN_TOKEN"));
        Results results = newResults(target, admin, operator, projectId, predecessors, cutoffs);
        Settlement settlement = new Settlement(results, token, admin, treasury, statement);
        StandingOrders orders = new StandingOrders(settlement);
        address groth16 = vm.envOr("CHAIN_GROTH16_VERIFIER", address(0));
        if (groth16 == address(0)) {
            groth16 = address(new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID));
        } else {
            require(groth16.code.length > 0, "CHAIN_GROTH16_VERIFIER has no code on this chain");
        }
        address mock = local
            ? address(new RiscZeroMockVerifier(bytes4(vm.envBytes32("CHAIN_MOCK_SELECTOR"))))
            : address(0);
        vm.stopBroadcast();

        console.log("token", address(token));
        console.log("results", address(results));
        console.log("settlement", address(settlement));
        console.log("standing orders", address(orders));
        console.log("groth16 verifier", groth16);
        // Every target's addresses, for config.yml (CHAIN_<TARGET>_*). The
        // devnet's, the testnet's and prod's are committed; local.json is
        // regenerated.
        {
            if (local) console.log("mock verifier", mock);
            string memory o = "deployment";
            vm.serializeAddress(o, "token", address(token));
            vm.serializeAddress(o, "results", address(results));
            vm.serializeAddress(o, "settlement", address(settlement));
            vm.serializeAddress(o, "standingOrders", address(orders));
            vm.serializeAddress(o, "groth16Verifier", groth16);
            vm.serializeUint(o, "chainId", block.chainid);
            vm.serializeUint(o, "deployBlock", block.number);
            string memory json = vm.serializeAddress(o, "mockVerifier", mock);
            vm.writeJson(json, string.concat("./deployments/", target, ".json"));
        }
    }

    /// Each target deploys only to its own chain.
    function checkChain(string memory target, uint256 chainId) internal pure {
        bytes32 t = keccak256(bytes(target));
        if (t == keccak256("local")) {
            require(chainId == 31_337, "local deploys only to Anvil");
        } else if (t == keccak256("devnet")) {
            require(chainId == 84_532, "devnet deploys only to Base Sepolia");
        } else if (t == keccak256("testnet")) {
            require(chainId == 84_532, "testnet deploys only to Base Sepolia");
        } else if (t == keccak256("prod")) {
            require(chainId == 8453, "prod deploys only to Base mainnet");
        } else {
            revert("CHAIN_TARGET is local, devnet, testnet or prod");
        }
    }

    /// The shortened durations exist only on the devnet: prod, the public
    /// testnet and local deploy `Results` itself, so the testnet and local
    /// work run prod's durations (OPERATIONS.md#targets).
    function newResults(
        string memory target,
        address admin,
        address operator,
        bytes32 projectId,
        address[] memory predecessors,
        uint256[] memory cutoffs
    ) internal returns (Results) {
        if (keccak256(bytes(target)) == keccak256("devnet")) {
            return new DevnetResults(admin, operator, projectId, predecessors, cutoffs);
        }
        return new Results(admin, operator, projectId, predecessors, cutoffs);
    }
}

/// The local target's admin steps: propose the local image, root, mock
/// verifier and genesis instruction, and switch betting on. The Taskfile then
/// warps Anvil past the 2-day timelock. Local only: elsewhere the admin key
/// signs these (by hand on the admin site for prod).
contract LocalSetup is Script {
    function run() external {
        require(block.chainid == 31_337, "local only");
        string memory json = vm.readFile("./deployments/local.json");
        Results results = Results(vm.parseJsonAddress(json, ".results"));
        Settlement settlement = Settlement(vm.parseJsonAddress(json, ".settlement"));
        address mock = vm.parseJsonAddress(json, ".mockVerifier");
        vm.startBroadcast(vm.envUint("CHAIN_ADMIN_KEY"));
        results.proposeImage(vm.envBytes32("CHAIN_LOCAL_IMAGE_DIGEST"));
        results.proposeRoot(vm.envBytes32("CHAIN_LOCAL_ROOT_HASH"));
        results.proposeVerifier(mock, vm.envBytes32("CHAIN_LOCAL_GUEST_IMAGE_ID"));
        results.orderInstruction();
        settlement.setBetting(true);
        vm.stopBroadcast();
    }
}
