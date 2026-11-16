//! The launch environment (TRUST.md#bootstrap-and-handover): the chain
//! binding, fixed for the life of the boot, and the runner's settings.
//!
//! In an image (`--cfg botstadia_image`) only the binding and the api's URL
//! come from the launch, which the image's launch policy enforces and its
//! public attestation token states; the RPC providers' keys come from the
//! api once registered, never the launch. The RPC hosts, the CDN, the
//! verifier, the guest image id, the project and the window are constants of
//! the image ([`crate::constants`]). The local target passes everything in.

use botstadia_chain::Address;

#[derive(Debug, Clone)]
pub struct Settings {
    pub api_url: String,
    pub chain_id: u64,
    pub results: Address,
    /// The allowlisted RPC endpoints. The runner acts only on what two of
    /// them agree on (TRUST.md#how-the-enclave-talks-to-the-chain).
    pub rpc_urls: Vec<String>,
    /// In an image, the chain whose keyed providers wait for the api's keys
    /// (`rpc_urls` holds only the keyless ones until then).
    pub keyed_chain: Option<&'static crate::constants::ChainConstants>,
    /// Where arenas and seed bots come from (the pinned CDN host; locally
    /// the CDN tree on disk).
    pub cdn: String,
    /// The batch window (ms): heartbeats and claims happen only at its
    /// boundaries.
    pub window_ms: u64,
    /// Jobs it holds at once.
    pub slots: usize,
    /// Our GCP project id, as attestation tokens name it.
    pub project: String,
    /// The verifier and zk guest image the boot proof is for (constants of
    /// the image; the local deploy's mock verifier locally).
    pub verifier: Address,
    pub guest_image_id: botstadia_chain::B256,
}

fn var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is not set"))
}

/// The variables a launch may set on an image: the image's launch policy
/// allows overriding exactly these (and the Dockerfile's label lists them),
/// and the attestation verifier accepts a token overriding no others.
pub const IMAGE_LAUNCH_VARS: &[&str] = botstadia_attestation_core::LAUNCH_VARS;

impl Settings {
    /// An image's settings: the launch's binding and api URL, everything
    /// else the image's own.
    #[cfg(botstadia_image)]
    pub fn from_env() -> anyhow::Result<Self> {
        Self::for_image(&|name| var(name))
    }

    /// The local target's settings, all from the environment.
    #[cfg(not(botstadia_image))]
    pub fn from_env() -> anyhow::Result<Self> {
        Self::local()
    }

    /// An image's settings from `var`.
    pub fn for_image(var: &dyn Fn(&str) -> anyhow::Result<String>) -> anyhow::Result<Self> {
        use crate::constants;
        let chain_id: u64 = var("BOTSTADIA_CHAIN_ID")?.parse()?;
        let chain = constants::chain(chain_id)?;
        Ok(Self {
            api_url: var("BOTSTADIA_API_URL")?,
            chain_id,
            results: var("BOTSTADIA_RESULTS")?.parse()?,
            rpc_urls: constants::keyless_urls(chain),
            keyed_chain: Some(chain),
            cdn: constants::CDN.into(),
            window_ms: constants::WINDOW_MS,
            slots: 1,
            project: constants::PROJECT.into(),
            verifier: chain.verifier.parse()?,
            guest_image_id: chain.guest_image_id.parse()?,
        })
    }

    #[cfg_attr(botstadia_image, allow(dead_code))]
    fn local() -> anyhow::Result<Self> {
        let rpc_urls: Vec<String> = var("BOTSTADIA_RPC_URLS")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        anyhow::ensure!(!rpc_urls.is_empty(), "BOTSTADIA_RPC_URLS names no endpoint");
        Ok(Self {
            api_url: var("BOTSTADIA_API_URL")?,
            chain_id: var("BOTSTADIA_CHAIN_ID")?.parse()?,
            results: var("BOTSTADIA_RESULTS")?.parse()?,
            rpc_urls,
            keyed_chain: None,
            cdn: var("BOTSTADIA_CDN")?,
            window_ms: std::env::var("BOTSTADIA_WINDOW_MS")
                .ok()
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(180_000),
            slots: std::env::var("BOTSTADIA_SLOTS")
                .ok()
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(1),
            project: var("BOTSTADIA_PROJECT")?,
            verifier: var("BOTSTADIA_VERIFIER")?.parse()?,
            guest_image_id: var("BOTSTADIA_GUEST_IMAGE_ID")?.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_takes_only_its_launch_vars() {
        let env = |name: &str| -> anyhow::Result<String> {
            assert!(
                IMAGE_LAUNCH_VARS.contains(&name),
                "{name} read from the launch"
            );
            Ok(match name {
                "BOTSTADIA_API_URL" => "https://api.example".into(),
                "BOTSTADIA_CHAIN_ID" => "84532".into(),
                "BOTSTADIA_RESULTS" => "0x40b96dF6D229d984Bc1400fA7A151c3423792E00".into(),
                _ => unreachable!(),
            })
        };
        match Settings::for_image(&env) {
            Ok(s) => {
                assert_eq!(
                    crate::constants::TARGET != crate::constants::Target::Prod,
                    s.chain_id == 84532
                );
                // The keyless one until the api hands over the keys.
                assert_eq!(s.rpc_urls, ["https://sepolia.base.org"]);
                assert!(s.keyed_chain.is_some());
                assert_eq!(s.cdn, crate::constants::CDN);
                assert_eq!(s.project, "botstadia");
            }
            // Prod's image serves no chain yet.
            Err(e) => {
                assert!(crate::constants::CHAINS.is_empty());
                assert!(e.to_string().contains("serves no chain"));
            }
        }
    }
}
