//! Constants of the image (OPERATIONS.md#configuration): fixed in the build,
//! so in its digest, never set at launch.
//!
//! The fast testnet's image is built with `--cfg botstadia_testnet`, which
//! shortens the waits that would hold a check for hours, and waits on Base's
//! `safe` head where prod's waits on `finalized`
//! (`botstadia_chain::rpc::BlockTag::SETTLED`; OPERATIONS.md#targets). It's
//! a different digest from prod's, and prod's image is never built with it;
//! the rehearsal runs prod's.

/// A new arena version is used by warm-ups and bettable sessions only this
/// long after the post announcing it (TRUST.md#approving-builds).
#[cfg(not(botstadia_testnet))]
pub const ARENA_WAIT_S: u64 = 24 * 60 * 60;
#[cfg(botstadia_testnet)]
pub const ARENA_WAIT_S: u64 = 10 * 60;

/// The contract's floor from a post to its sessions' close
/// (`CLOSE_MIN_AFTER_POST`): a first bet proves against a post at least
/// this long before close.
#[cfg(not(botstadia_testnet))]
pub const CLOSE_MIN_AFTER_POST_S: u64 = 60 * 60;
#[cfg(botstadia_testnet)]
pub const CLOSE_MIN_AFTER_POST_S: u64 = 2 * 60;

/// The contract's minimum anchor age before a result (`RESULT_MIN_AGE`).
#[cfg(not(botstadia_testnet))]
pub const RESULT_MIN_AGE_S: u64 = 15 * 60;
#[cfg(botstadia_testnet)]
pub const RESULT_MIN_AGE_S: u64 = 2 * 60;

/// A session's result deadline after its close (`RESULT_DEADLINE`).
#[cfg(not(botstadia_testnet))]
pub const RESULT_DEADLINE_S: u64 = 24 * 60 * 60;
#[cfg(botstadia_testnet)]
pub const RESULT_DEADLINE_S: u64 = 2 * 60 * 60;

/// A first bet proves against a post at most this long before close, so a
/// bettable session looks back no further (TRUST.md#how-the-enclave-talks-
/// to-the-chain).
pub const POST_LOOKBACK_S: u64 = 7 * 24 * 60 * 60;

/// Whether this is the fast testnet's build.
pub const FAST_TESTNET: bool = cfg!(botstadia_testnet);

/// Our GCP project id, as attestation tokens name it (SETUP: one project
/// for testnet and prod; the chain binding keeps them apart).
pub const PROJECT: &str = "botstadia";

/// The CDN host arenas and seed bots come from, each file checked against
/// its named digest (TRUST.md#arenas).
pub const CDN: &str = "https://cdn.botstadia.mediabucket.io";

/// The batch window (ms): heartbeats and claims only at its boundaries. A
/// round waits on it up to three times (the claim, the anchor, the result),
/// so the fast testnet's is a minute.
#[cfg(not(botstadia_testnet))]
pub const WINDOW_MS: u64 = 180_000;
#[cfg(botstadia_testnet)]
pub const WINDOW_MS: u64 = 60_000;

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

/// Base Sepolia: the fast testnet.
pub const BASE_SEPOLIA: ChainConstants = ChainConstants {
    chain_id: 84532,
    // In the order the runner pairs them: two that agree are enough, and
    // the free tiers' daily allowances run out one at a time.
    providers: &[
        Provider {
            name: "alchemy",
            url: "https://base-sepolia.g.alchemy.com/v2/{key}",
        },
        Provider {
            name: "base",
            url: "https://sepolia.base.org",
        },
        Provider {
            name: "infura",
            url: "https://base-sepolia.infura.io/v3/{key}",
        },
        Provider {
            name: "drpc",
            url: "https://lb.drpc.org/ogrpc?network=base-sepolia&dkey={key}",
        },
    ],
    verifier: "0xe403B9a3B6aD517E1ACFeb3C05824A5cE808a423",
    guest_image_id: GUEST_IMAGE_ID,
};

/// The chains this image serves: the testnet's build serves only the
/// testnet, and prod's serves none until its contracts are deployed (its
/// verifier goes here then, and a new image is approved through the
/// timelock).
#[cfg(botstadia_testnet)]
pub const CHAINS: &[ChainConstants] = &[BASE_SEPOLIA];
#[cfg(not(botstadia_testnet))]
pub const CHAINS: &[ChainConstants] = &[];

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
        assert_eq!(urls[0], "https://base-sepolia.g.alchemy.com/v2/a1");
        assert_eq!(urls[1], "https://sepolia.base.org");
        assert_eq!(
            urls[3],
            "https://lb.drpc.org/ogrpc?network=base-sepolia&dkey=c-3"
        );
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

    #[test]
    fn an_image_serves_only_its_own_chains() {
        assert_eq!(chain(84532).is_ok(), FAST_TESTNET);
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
        let keeper = if super::FAST_TESTNET {
            Durations::FAST
        } else {
            Durations::PROD
        };
        assert_eq!(keeper.arena_wait_s, super::ARENA_WAIT_S);
        assert_eq!(keeper.close_min_after_post_s, super::CLOSE_MIN_AFTER_POST_S);
        assert_eq!(keeper.result_deadline_s, super::RESULT_DEADLINE_S);
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
