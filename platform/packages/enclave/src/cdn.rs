//! Arenas and seed bots in plaintext, from the CDN host pinned in the image
//! (TRUST.md#seed-bots, TRUST.md#bundle-encryption); locally the CDN tree on
//! disk. Every file is checked against the digest it was asked for, so the
//! arena slug a payload names (from the api) can only make a fetch fail,
//! never change what's seated.

use std::path::PathBuf;

pub trait Cdn {
    /// An arena engine by its arena's slug and its digest (`sha256-<hex>`).
    fn arena(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>>;
    /// A seed bot's plaintext bundle ZIP by its arena's slug and its digest.
    fn seed_bot(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>>;
}

/// The CDN tree on disk: `arena/catalog/<slug>/engine/<digest>.wasm` and
/// `bot/catalog/<slug>/<name>/<digest>.zip`.
pub struct DirCdn(pub PathBuf);

fn checked(bytes: Vec<u8>, digest: &str) -> anyhow::Result<Vec<u8>> {
    let got = crate::digest(&bytes);
    anyhow::ensure!(got == digest, "the CDN served {got} for {digest}");
    Ok(bytes)
}

impl DirCdn {
    fn find(&self, top: &str, depth: usize, file: &str) -> anyhow::Result<PathBuf> {
        let mut dirs = vec![self.0.join(top).join("catalog")];
        for _ in 0..depth {
            dirs = dirs
                .iter()
                .filter_map(|d| std::fs::read_dir(d).ok())
                .flat_map(|rd| rd.flatten().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
        }
        dirs.iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
            .ok_or_else(|| anyhow::anyhow!("{file} isn't in the CDN tree"))
    }
}

impl Cdn for DirCdn {
    fn arena(&self, _arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        // arena/catalog/<slug>/engine/<digest>.wasm
        let path = self.find("arena", 2, &format!("{digest}.wasm"))?;
        checked(std::fs::read(path)?, digest)
    }

    fn seed_bot(&self, _arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        // bot/catalog/<slug>/<name>/<digest>.zip
        let path = self.find("bot", 2, &format!("{digest}.zip"))?;
        checked(std::fs::read(path)?, digest)
    }
}

/// The most one CDN file may be: far above any arena engine or bundle.
pub const CDN_LIMIT_BYTES: u64 = 256 << 20;

/// The pinned CDN host over HTTPS (`constants::CDN` in an image), laid out
/// as the tree on disk. A seed bot's file is found through its arena's bot
/// catalog (`bot/catalog/<slug>/latest.json`).
pub struct HttpCdn {
    pub base: String,
}

/// A slug the URL can carry as one path segment, nothing else.
fn slug(arena_id: &str) -> anyhow::Result<&str> {
    anyhow::ensure!(
        !arena_id.is_empty()
            && arena_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "{arena_id:?} isn't an arena slug"
    );
    Ok(arena_id)
}

fn is_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256-")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl HttpCdn {
    fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{path}", self.base.trim_end_matches('/'));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(crate::client::CONNECT_TIMEOUT))
            .timeout_global(Some(crate::client::CALL_TIMEOUT))
            .build()
            .into();
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|e| anyhow::anyhow!("fetching {path} from the CDN: {e}"))?;
        Ok(response
            .body_mut()
            .with_config()
            .limit(CDN_LIMIT_BYTES)
            .read_to_vec()?)
    }

    /// The seed bot's file in its arena's bot catalog, as a path the URL can
    /// carry: `<name>/<digest>.zip`.
    fn seed_bot_file(catalog: &[u8], digest: &str) -> anyhow::Result<String> {
        let catalog: serde_json::Value = serde_json::from_slice(catalog)?;
        let file = catalog["bots"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|b| b["digest"].as_str() == Some(digest))
            .and_then(|b| b["file"].as_str())
            .ok_or_else(|| anyhow::anyhow!("{digest} isn't in its arena's bot catalog"))?;
        let ok = file.split('/').count() == 2
            && file.ends_with(&format!("/{digest}.zip"))
            && slug(file.split('/').next().unwrap_or_default()).is_ok();
        anyhow::ensure!(ok, "the bot catalog names an odd file for {digest}");
        Ok(file.to_string())
    }
}

impl Cdn for HttpCdn {
    fn arena(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(is_digest(digest), "{digest:?} isn't a digest");
        let bytes = self.get(&format!(
            "arena/catalog/{}/engine/{digest}.wasm",
            slug(arena_id)?
        ))?;
        checked(bytes, digest)
    }

    fn seed_bot(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(is_digest(digest), "{digest:?} isn't a digest");
        let slug = slug(arena_id)?;
        let catalog = self.get(&format!("bot/catalog/{slug}/latest.json"))?;
        let file = Self::seed_bot_file(&catalog, digest)?;
        checked(self.get(&format!("bot/catalog/{slug}/{file}"))?, digest)
    }
}

/// The CDN a runner reads: the pinned host over HTTPS in an image, the tree
/// on disk locally.
pub enum AnyCdn {
    Dir(DirCdn),
    Http(HttpCdn),
}

impl AnyCdn {
    pub fn new(cdn: &str) -> Self {
        if cdn.starts_with("https://") {
            Self::Http(HttpCdn { base: cdn.into() })
        } else {
            Self::Dir(DirCdn(cdn.into()))
        }
    }
}

impl Cdn for AnyCdn {
    fn arena(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Dir(c) => c.arena(arena_id, digest),
            Self::Http(c) => c.arena(arena_id, digest),
        }
    }

    fn seed_bot(&self, arena_id: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Dir(c) => c.seed_bot(arena_id, digest),
            Self::Http(c) => c.seed_bot(arena_id, digest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_bot_is_found_through_its_arena_s_catalog_and_nothing_else() {
        let d = format!("sha256-{}", "ab".repeat(32));
        let catalog = |file: &str| {
            serde_json::to_vec(&serde_json::json!({"bots": [{"digest": d, "file": file}]})).unwrap()
        };
        assert_eq!(
            HttpCdn::seed_bot_file(&catalog(&format!("scripted/{d}.zip")), &d).unwrap(),
            format!("scripted/{d}.zip")
        );
        for odd in [
            format!("../x/{d}.zip"),
            format!("a/b/{d}.zip"),
            "scripted/other.zip".to_string(),
            format!("Up/{d}.zip"),
        ] {
            assert!(HttpCdn::seed_bot_file(&catalog(&odd), &d).is_err(), "{odd}");
        }
        assert!(
            HttpCdn::seed_bot_file(&catalog("x"), &format!("sha256-{}", "cd".repeat(32))).is_err()
        );
        assert!(slug("racing").is_ok() && slug("dance-off").is_ok());
        assert!(slug("../x").is_err() && slug("").is_err() && slug("a/b").is_err());
        assert!(matches!(
            AnyCdn::new("https://cdn.example"),
            AnyCdn::Http(_)
        ));
        assert!(matches!(AnyCdn::new("/tmp/cdn"), AnyCdn::Dir(_)));
    }

    /// The pinned host serves an arena's engine and a seed bot, each matching
    /// its digest, as an image reads them.
    #[test]
    #[ignore = "needs the network: reads the real CDN"]
    fn the_pinned_cdn_serves_an_arena_and_a_seed_bot() {
        let cdn = HttpCdn {
            base: crate::constants::CDN.into(),
        };
        let arenas: serde_json::Value =
            serde_json::from_slice(&cdn.get("arena/catalog/racing/latest.json").unwrap()).unwrap();
        let arena = arenas["versions"][0]["digest"].as_str().unwrap();
        assert!(!cdn.arena("racing", arena).unwrap().is_empty());
        let bots: serde_json::Value =
            serde_json::from_slice(&cdn.get("bot/catalog/racing/latest.json").unwrap()).unwrap();
        let bot = bots["bots"][0]["digest"].as_str().unwrap();
        assert!(!cdn.seed_bot("racing", bot).unwrap().is_empty());
        assert!(cdn.seed_bot("boxing", bot).is_err());
    }
}
