//! Constants of the image (OPERATIONS.md#configuration): fixed in the build,
//! so in its digest, never set at launch.
//!
//! One image per target (OPERATIONS.md#targets), chosen at build time:
//!
//! - **prod** (no target cfg): prod's waits, serving Base mainnet once its
//!   contracts exist.
//! - **testnet** (`--cfg botstadia_testnet`): prod's waits exactly, serving
//!   Base Sepolia. The cfg chooses only the chain ([`chains`]), so the public
//!   testnet runs what prod runs.
//! - **devnet** (`--cfg botstadia_devnet`): shortened waits, so a check never
//!   holds for hours, and Base's `safe` head where the others wait on
//!   `finalized` (`botstadia_chain::rpc::BlockTag::SETTLED`), serving Base
//!   Sepolia. Never a release.

/// Which target's image this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Prod,
    Testnet,
    Devnet,
}

#[cfg(all(botstadia_testnet, botstadia_devnet))]
compile_error!("an image is built for one target: botstadia_testnet or botstadia_devnet");

/// This image's target.
pub const TARGET: Target = if cfg!(botstadia_devnet) {
    Target::Devnet
} else if cfg!(botstadia_testnet) {
    Target::Testnet
} else {
    Target::Prod
};

/// The waits an image keeps, each matching a contract constant or the
/// keeper's (`botstadia_backend::durations`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// A new arena version is used by warm-ups and bettable sessions only
    /// this long after the post announcing it (TRUST.md#approving-builds).
    pub arena_wait_s: u64,
    /// The contract's floor from a post to its sessions' close
    /// (`CLOSE_MIN_AFTER_POST`): a first bet proves against a post at least
    /// this long before close.
    pub close_min_after_post_s: u64,
    /// The contract's minimum anchor age before a result (`RESULT_MIN_AGE`).
    pub result_min_age_s: u64,
    /// A session's result deadline after its close (`RESULT_DEADLINE`).
    pub result_deadline_s: u64,
    /// The batch window (ms): heartbeats and claims only at its boundaries.
    /// A round waits on it up to three times (the claim, the anchor, the
    /// result), so the devnet's is a minute.
    pub window_ms: u64,
}

/// Prod's waits: `Results`' constants. The testnet's image keeps them.
pub const PROD_WAITS: Waits = Waits {
    arena_wait_s: 24 * 60 * 60,
    close_min_after_post_s: 60 * 60,
    result_min_age_s: 15 * 60,
    result_deadline_s: 24 * 60 * 60,
    window_ms: 180_000,
};

/// The devnet's: `DevnetResults`' constants.
pub const DEVNET_WAITS: Waits = Waits {
    arena_wait_s: 10 * 60,
    close_min_after_post_s: 2 * 60,
    result_min_age_s: 2 * 60,
    result_deadline_s: 2 * 60 * 60,
    window_ms: 60_000,
};

/// A target's waits: prod's everywhere but the devnet.
pub const fn waits(target: Target) -> Waits {
    match target {
        Target::Devnet => DEVNET_WAITS,
        Target::Prod | Target::Testnet => PROD_WAITS,
    }
}

const WAITS: Waits = waits(TARGET);

/// This image's arena wait (see [`Waits`]).
pub const ARENA_WAIT_S: u64 = WAITS.arena_wait_s;
/// This image's close floor (see [`Waits`]).
pub const CLOSE_MIN_AFTER_POST_S: u64 = WAITS.close_min_after_post_s;
/// This image's minimum result age (see [`Waits`]).
pub const RESULT_MIN_AGE_S: u64 = WAITS.result_min_age_s;
/// This image's result deadline (see [`Waits`]).
pub const RESULT_DEADLINE_S: u64 = WAITS.result_deadline_s;
/// This image's batch window (see [`Waits`]).
pub const WINDOW_MS: u64 = WAITS.window_ms;

/// A first bet proves against a post at most this long before close, so a
/// bettable session looks back no further (TRUST.md#how-the-enclave-talks-
/// to-the-chain).
pub const POST_LOOKBACK_S: u64 = 7 * 24 * 60 * 60;

/// Whether this is the devnet's build: its shortened waits and the `safe`
/// head.
pub const DEVNET: bool = matches!(TARGET, Target::Devnet);

/// Our GCP project id, as attestation tokens name it (SETUP: one project
/// for testnet and prod; the chain binding keeps them apart).
pub const PROJECT: &str = "botstadia";

/// The CDN host arenas and seed bots come from, each file checked against
/// its named digest (TRUST.md#arenas).
pub const CDN: &str = "https://cdn.botstadia.mediabucket.io";

/// One allowlisted RPC provider: its URL with `{key}` where the launch's
/// key for it goes, or none for a keyless public endpoint. The host is fixed
/// here, so a launch chooses keys, never hosts
/// (TRUST.md#how-the-enclave-talks-to-the-chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub name: &'static str,
    pub url: &'static str,
}

/// What an image holds for one chain it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainConstants {
    pub chain_id: u64,
    pub providers: &'static [Provider],
    /// The Groth16 verifier and the zk guest image id the boot proof is for.
    pub verifier: &'static str,
    pub guest_image_id: &'static str,
}

/// The zk guest's image id, rebuilt reproducibly from its source.
pub const GUEST_IMAGE_ID: &str =
    "0x61b336815488909ed1e6fd48ad779f12ea443c9d86c4446bce0332e968294228";

/// Base Sepolia: the testnet and the devnet, each its own chain binding
/// (results contract) on the one chain, sharing one Groth16 verifier.
pub const BASE_SEPOLIA: ChainConstants = ChainConstants {
    chain_id: 84532,
    // In the order they're asked: the first to answer, and for agreement the
    // first two. The keyless one first, then the largest free allowance, so
    // no keyed allowance pays for every wait.
    providers: &[
        Provider {
            name: "base",
            url: "https://sepolia.base.org",
        },
        Provider {
            name: "drpc",
            url: "https://lb.drpc.org/ogrpc?network=base-sepolia&dkey={key}",
        },
        Provider {
            name: "infura",
            url: "https://base-sepolia.infura.io/v3/{key}",
        },
        Provider {
            name: "alchemy",
            url: "https://base-sepolia.g.alchemy.com/v2/{key}",
        },
    ],
    verifier: "0xe403B9a3B6aD517E1ACFeb3C05824A5cE808a423",
    guest_image_id: GUEST_IMAGE_ID,
};

/// The chains a target's image serves: Base Sepolia for the testnet and
/// the devnet, and none for prod until its contracts are deployed (its
/// verifier goes here then, and a new image is approved through the
/// timelock).
pub const fn chains(target: Target) -> &'static [ChainConstants] {
    match target {
        Target::Testnet | Target::Devnet => &[BASE_SEPOLIA],
        Target::Prod => &[],
    }
}

/// The chains this image serves.
pub const CHAINS: &[ChainConstants] = chains(TARGET);

/// This image's constants for `chain_id`, or why it won't serve it.
pub fn chain(chain_id: u64) -> anyhow::Result<&'static ChainConstants> {
    CHAINS
        .iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| anyhow::anyhow!("this image serves no chain {chain_id}"))
}

/// The allowlisted providers that need no key: enough to read block time
/// before registering, never two to agree on.
pub fn keyless_urls(chain: &ChainConstants) -> Vec<String> {
    chain
        .providers
        .iter()
        .filter(|p| !p.url.contains("{key}"))
        .map(|p| p.url.to_string())
        .collect()
}

/// The allowlisted URLs from the api's keys, `name=key` pairs separated
/// by commas. Every keyed provider needs its key: at least two must agree on
/// everything the enclave reads, and one outage must never stop it.
pub fn rpc_urls(chain: &ChainConstants, keys: &str) -> anyhow::Result<Vec<String>> {
    let keys: std::collections::BTreeMap<&str, &str> = keys
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            p.trim()
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("an RPC key is `name=key`"))
        })
        .collect::<anyhow::Result<_>>()?;
    for name in keys.keys() {
        anyhow::ensure!(
            chain.providers.iter().any(|p| p.name == *name),
            "{name} isn't an allowlisted provider for chain {}",
            chain.chain_id
        );
    }
    chain
        .providers
        .iter()
        .map(|p| {
            if !p.url.contains("{key}") {
                return Ok(p.url.to_string());
            }
            let key = keys
                .get(p.name)
                .ok_or_else(|| anyhow::anyhow!("no RPC key for {}", p.name))?;
            anyhow::ensure!(
                !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "the {} key isn't a plain key",
                p.name
            );
            Ok(p.url.replace("{key}", key))
        })
        .collect()
}

#[cfg(test)]
mod constant_tests {
    use super::*;

    #[test]
    fn a_launch_chooses_keys_never_hosts() {
        let urls = rpc_urls(&BASE_SEPOLIA, "alchemy=a1, infura=b2,drpc=c-3").unwrap();
        assert_eq!(urls[0], "https://sepolia.base.org");
        assert_eq!(
            urls[1],
            "https://lb.drpc.org/ogrpc?network=base-sepolia&dkey=c-3"
        );
        assert_eq!(urls[3], "https://base-sepolia.g.alchemy.com/v2/a1");
        // A key that would bend the URL, an unknown provider, a missing key.
        assert!(rpc_urls(&BASE_SEPOLIA, "alchemy=x@evil.com/,infura=b,drpc=c").is_err());
        assert!(rpc_urls(&BASE_SEPOLIA, "alchemy=a,infura=b,drpc=c,evil=d").is_err());
        assert!(rpc_urls(&BASE_SEPOLIA, "alchemy=a,infura=b").is_err());
    }

    #[test]
    fn every_provider_is_https_with_at_most_one_key_slot() {
        for p in BASE_SEPOLIA.providers {
            assert!(p.url.starts_with("https://"), "{}", p.name);
            assert!(p.url.matches("{key}").count() <= 1, "{}", p.name);
        }
        assert!(BASE_SEPOLIA.providers.len() >= 3);
    }

    /// The image's constants are the ones config.yml deploys with.
    #[test]
    fn the_constants_match_config_yml() {
        let config = include_str!("../../../config.yml");
        let value = |key: &str| {
            config
                .lines()
                .find_map(|l| l.trim().strip_prefix(&format!("{key}: ")))
                .map(|v| v.trim().trim_matches('"').to_string())
                .unwrap_or_else(|| panic!("{key} not in config.yml"))
        };
        assert_eq!(value("CHAIN_GUEST_IMAGE_ID"), GUEST_IMAGE_ID);
        assert_eq!(
            value("CHAIN_TESTNET_GROTH16_VERIFIER"),
            BASE_SEPOLIA.verifier
        );
        assert_eq!(value("CHAIN_ID_TESTNET"), BASE_SEPOLIA.chain_id.to_string());
        assert_eq!(value("CHAIN_PROJECT_ID_TESTNET"), PROJECT);
        assert_eq!(value("URL_CDN"), CDN);
    }

    /// The public testnet runs what prod runs: its image differs from
    /// prod's only in the chain it serves (OPERATIONS.md#the-testnet).
    #[test]
    fn the_testnets_image_keeps_prods_constants() {
        assert_eq!(waits(Target::Testnet), waits(Target::Prod));
        assert_eq!(waits(Target::Prod), PROD_WAITS);
        assert_ne!(waits(Target::Devnet), PROD_WAITS);
        assert_eq!(chains(Target::Testnet), &[BASE_SEPOLIA]);
        assert_eq!(chains(Target::Devnet), &[BASE_SEPOLIA]);
        assert!(chains(Target::Prod).is_empty());
        // The image's Dockerfile builds each target with its own cfg alone:
        // the testnet's never carries the devnet's.
        let dockerfile = include_str!("../image/Dockerfile");
        let cfg = |target: &str| {
            dockerfile
                .lines()
                .find_map(|l| l.trim().strip_prefix(&format!("{target}) cfg=\"")))
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("no {target} case in the Dockerfile"))
                .to_string()
        };
        assert_eq!(cfg("prod"), "--cfg botstadia_image");
        assert_eq!(
            cfg("testnet"),
            "--cfg botstadia_image --cfg botstadia_testnet"
        );
        assert_eq!(
            cfg("devnet"),
            "--cfg botstadia_image --cfg botstadia_devnet"
        );
    }

    #[test]
    fn an_image_serves_only_its_own_chains() {
        assert_eq!(chain(84532).is_ok(), TARGET != Target::Prod);
        assert!(chain(8453).is_err());
        assert!(chain(31337).is_err());
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    /// The keeper waits as long as the image it posts for.
    #[test]
    fn the_keeper_and_the_image_agree_on_the_arena_wait() {
        use botstadia_backend::durations::Durations;
        let keeper = if super::DEVNET {
            Durations::DEVNET
        } else {
            Durations::PROD
        };
        assert_eq!(keeper.arena_wait_s, super::ARENA_WAIT_S);
        assert_eq!(keeper.close_min_after_post_s, super::CLOSE_MIN_AFTER_POST_S);
        assert_eq!(keeper.result_deadline_s, super::RESULT_DEADLINE_S);
        assert_eq!(Durations::PROD.arena_wait_s, super::PROD_WAITS.arena_wait_s);
        assert_eq!(
            Durations::PROD.close_min_after_post_s,
            super::PROD_WAITS.close_min_after_post_s
        );
        assert_eq!(
            Durations::PROD.result_deadline_s,
            super::PROD_WAITS.result_deadline_s
        );
        assert_eq!(
            Durations::DEVNET.arena_wait_s,
            super::DEVNET_WAITS.arena_wait_s
        );
        assert_eq!(
            Durations::DEVNET.close_min_after_post_s,
            super::DEVNET_WAITS.close_min_after_post_s
        );
        assert_eq!(
            Durations::DEVNET.result_deadline_s,
            super::DEVNET_WAITS.result_deadline_s
        );
    }

    /// The keeper schedules new sessions under the host's newest budget
    /// version. Here, not in the host, so the host needs nothing private.
    #[test]
    fn the_keeper_schedules_under_the_newest_budget_version() {
        assert_eq!(
            botstadia_host::budget::current().id,
            botstadia_backend::keeper::BUDGET_VERSION
        );
    }

    /// The api offers every post the enclave looks back over.
    #[test]
    fn the_api_and_the_image_look_back_as_far() {
        assert_eq!(
            botstadia_backend::posts::POST_LOOKBACK_S,
            super::POST_LOOKBACK_S
        );
    }
}
