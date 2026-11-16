//! A local api in-process (feature `testing`): the machine routes on SQLite
//! with the real migrations, the local chain config, and the mock root and
//! image approved in the index, as the local deploy leaves them.

use botstadia_attestation_core::{LOCAL_MOCK_ROOT_HASH, mock};
use botstadia_backend::chain_config::{ChainConfig, Target};
use botstadia_backend::index::{self, Indexed};
use botstadia_backend::testing::{Memory, Sqlite};
use botstadia_chain::Address;

use crate::client::InProcess;

pub const CHAIN_ID: u64 = 31_337;

/// An arena's newest version in the CDN tree (`arena/catalog/<slug>/
/// latest.json`), as its `<digest>.wasm` engine file name: what its current
/// bots are built for. Never a directory listing, whose order differs
/// between macOS and Linux.
pub fn newest_arena(cdn: &std::path::Path, slug: &str) -> String {
    let latest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(cdn.join(format!("arena/catalog/{slug}/latest.json")))
            .expect("the arena's latest.json"),
    )
    .expect("latest.json is JSON");
    format!(
        "{}.wasm",
        latest["versions"][0]["digest"]
            .as_str()
            .expect("a newest version")
    )
}

pub fn results() -> Address {
    Address::repeat_byte(0x5e)
}

pub fn chain() -> ChainConfig {
    ChainConfig {
        target: Target::Local,
        chain_id: CHAIN_ID,
        rpc_url: String::new(),
        results: format!("{:#x}", results()),
        settlement: format!("{:#x}", Address::repeat_byte(0x5f)),
        standing_orders: format!("{:#x}", Address::repeat_byte(0x60)),
        project: mock::PROJECT.into(),
        admin: format!("{:#x}", Address::repeat_byte(0x61)),
        durations: botstadia_backend::durations::Durations::PROD,
        start_block: 0,
        log_range: 2_000,
        verifier: None,
        guest_image_id: None,
        enclave_rpc_keys: Some("alchemy=a,infura=b,drpc=c".into()),
    }
}

pub async fn local_api(now_ms: u64) -> InProcess {
    let db = Sqlite::migrated();
    let root = format!("0x{}", hex::encode(LOCAL_MOCK_ROOT_HASH));
    let image = format!("0x{}", mock::IMAGE_DIGEST.trim_start_matches("sha256:"));
    index::record(
        &db,
        1,
        0,
        "0x",
        "0x",
        &Indexed::RootProposed { root, ready_at: 0 },
    )
    .await
    .unwrap();
    index::record(
        &db,
        1,
        1,
        "0x",
        "0x",
        &Indexed::ImageProposed {
            digest: image,
            ready_at: 0,
        },
    )
    .await
    .unwrap();
    InProcess {
        db,
        blobs: Memory::default(),
        archives: Memory::default(),
        chain: chain(),
        now: std::cell::Cell::new(now_ms),
    }
}

/// The BundleKey in force (instruction 1 filled with `public`) and a racing
/// arena version posted long ago, as the local deploy and the keeper's index
/// would leave them.
pub async fn key_and_arena(api: &InProcess, public: [u8; 32], arena_digest: &str) {
    index::record(
        &api.db,
        2,
        0,
        "0x",
        "0x",
        &Indexed::InstructionProposed {
            number: 1,
            ready_at: 0,
        },
    )
    .await
    .unwrap();
    index::record(
        &api.db,
        2,
        1,
        "0x",
        "0x",
        &Indexed::KeyApproved {
            key: "0x".into(),
            image: "0x".into(),
            root: "0x".into(),
            verifier: "0x".into(),
            instruction: 1,
            bundle_key: format!("0x{}", hex::encode(public)),
        },
    )
    .await
    .unwrap();
    use botstadia_backend::db::Db;
    api.db
        .execute(
            "INSERT INTO arenas (digest, arena_id, version, payload_schema, declaration_json, admin_signature, published_at, posted_in, posted_at) VALUES (?1, 'racing', '1', 1, '{}', '0x', 1, 1, 1)",
            &[arena_digest.into()],
        )
        .await
        .unwrap();
}

/// Upload a bundle as a builder would (sealed to the BundleKey and an author
/// key), through the api's own upload service. Returns the bot id and the
/// author secret.
pub async fn upload(
    api: &InProcess,
    bundle_public: [u8; 32],
    zip: &[u8],
    user: &str,
) -> (String, [u8; 32]) {
    use botstadia_upload::seal::{Sealed, seal_bundle, x25519_generate};
    let (author_secret, author_public) = x25519_generate().unwrap();
    let sealed = seal_bundle(zip, &bundle_public, &author_public).unwrap();
    let user_id = botstadia_backend::testing::user(&api.db, user).await;
    let req = botstadia_shared::platform::BundleUpload {
        name: format!("bot-{user}"),
        arena_id: "racing".into(),
        payload_schema: 1,
        kind: "rust".into(),
        digest: sealed.digest.clone(),
        author_address: format!("{:#x}", Address::repeat_byte(0xa1)),
        author_x25519: format!("0x{}", hex::encode(author_public)),
        bundle_key_pub: format!("0x{}", hex::encode(bundle_public)),
        instruction: 1,
        wrapped_bundle_key: Sealed::b64(&sealed.wrapped_bundle_key),
        wrapped_author_key: Sealed::b64(&sealed.wrapped_author_key),
        ciphertext: Sealed::b64(&sealed.ciphertext),
    };
    let ids = botstadia_backend::bots::Ids {
        bot: rand16(),
        job: rand16(),
    };
    let receipt = botstadia_backend::bots::upload(
        &api.db,
        &api.blobs,
        &chain().binding(),
        &user_id,
        &req,
        ids,
        api.now.get(),
    )
    .await
    .unwrap()
    .unwrap_or_else(|r| panic!("upload refused: {r:?}"));
    (receipt.bot_id, author_secret)
}

fn rand16() -> [u8; 16] {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).unwrap();
    b
}
