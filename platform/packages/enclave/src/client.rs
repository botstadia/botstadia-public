//! The api, as a courier (TRUST.md#bootstrap-and-handover): a boot registers
//! on its token, then signs every request with its SubmitKey over the nonce
//! the api last gave it. The api is never trusted for chain state or keys.

use botstadia_chain::sign::Key;
use botstadia_shared::api::Endpoint;
use botstadia_shared::error::ApiError;
use botstadia_shared::platform::{
    Answer, FleetRegister, Registered, Registration, Signed, machine_message,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// How requests reach the api.
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ApiError>;
}

/// The largest answer the api may give: a whole bundle's ciphertext, as
/// base64 inside JSON (BOTS.md allows a 25 MB bundle), with room to spare.
/// ureq's own default, 10 MB, would refuse any bundle past about 7.4 MB.
pub const RESPONSE_LIMIT_BYTES: u64 =
    (botstadia_interface::bundle::MAX_BUNDLE_BYTES + (1 << 20)) / 3 * 4 + (4 << 20);

/// How long a call may take to connect, and in all. Every call holds the
/// machine's nonce, so one stalled connection would otherwise hang the
/// runner. The overall limit fits the largest answer on a slow link.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The api over HTTP.
pub struct Http {
    pub base: String,
    /// [`CALL_TIMEOUT`] outside tests.
    pub timeout: std::time::Duration,
}

impl Http {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            timeout: CALL_TIMEOUT,
        }
    }
}

impl Transport for Http {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
                .timeout_global(Some(timeout))
                .build()
                .into();
            let mut response = agent
                .post(&url)
                .header("content-type", "application/json")
                .send(body.to_string())
                .map_err(|_| ApiError::Internal)?;
            let status = response.status().as_u16();
            let text = response
                .body_mut()
                .with_config()
                .limit(RESPONSE_LIMIT_BYTES)
                .read_to_string()
                .map_err(|_| ApiError::Internal)?;
            if status >= 400 {
                return Err(serde_json::from_str::<ApiError>(&text).unwrap_or(ApiError::Internal));
            }
            serde_json::from_str(&text).map_err(|_| ApiError::Internal)
        })
        .await
        .map_err(|_| ApiError::Internal)?
    }
}

/// The machine routes answered in-process on SQLite (feature `testing`).
#[cfg(feature = "testing")]
pub struct InProcess {
    pub db: botstadia_backend::testing::Sqlite,
    pub blobs: botstadia_backend::testing::Memory,
    pub archives: botstadia_backend::testing::Memory,
    pub chain: botstadia_backend::chain_config::ChainConfig,
    pub now: std::cell::Cell<u64>,
}

#[cfg(feature = "testing")]
impl Transport for InProcess {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        let mut ids = || {
            let mut b = [0u8; 16];
            getrandom::fill(&mut b).expect("randomness");
            b
        };
        botstadia_backend::machine::dispatch(
            &self.db,
            &self.blobs,
            &self.archives,
            &self.chain,
            path,
            body,
            &mut ids,
            self.now.get(),
            &|line| eprintln!("api: {line}"),
        )
        .await
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Takes a fresh registration token (block time in seconds → token).
pub type Reregister = Box<dyn Fn() -> anyhow::Result<String> + Send + Sync>;

/// A registered boot's signed client.
pub struct Machine<T> {
    pub transport: T,
    key: Key,
    address: String,
    chain_id: u64,
    results: String,
    /// Registering again on a fresh token resets the api's nonce: the way
    /// back if a refusal ever leaves the two out of step.
    reregister: Option<Reregister>,
    /// The api's last nonce, held for the whole of each call: requests from
    /// the runner and its job threads go one at a time, each over the nonce
    /// the last one returned.
    nonce: tokio::sync::Mutex<String>,
}

impl<T: Transport> Machine<T> {
    /// Register on the token alone; returns the client and the api's hint of
    /// where the BundleKey is held.
    pub async fn register(
        transport: T,
        key: Key,
        token: String,
        chain_id: u64,
        results: &str,
    ) -> anyhow::Result<(Self, Registered)> {
        let address = format!("{:#x}", key.address());
        let body = serde_json::to_value(Registration {
            address: address.clone(),
            token,
            chain_id,
            results: results.to_string(),
        })?;
        let registered: Registered = serde_json::from_value(
            transport
                .post(FleetRegister::PATH, body)
                .await
                .map_err(|e| anyhow::anyhow!("registration refused: {e:?}"))?,
        )?;
        let nonce = tokio::sync::Mutex::new(registered.nonce.clone());
        Ok((
            Self {
                transport,
                key,
                address,
                chain_id,
                results: results.to_string(),
                reregister: None,
                nonce,
            },
            registered,
        ))
    }

    /// How to take a fresh registration token, for resynchronising.
    pub fn set_reregister(&mut self, f: Reregister) {
        self.reregister = Some(f);
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    /// One signed machine request.
    pub async fn call<E, B, R>(&self, body: B) -> anyhow::Result<R>
    where
        E: Endpoint<Req = Signed<B>, Res = Answer<R>>,
        B: Serialize + Clone,
        R: DeserializeOwned,
    {
        // The api hashes the body as it re-serializes what it parsed: a
        // struct in field order, a JSON value with sorted keys. Serializing
        // the body as typed gives the same bytes either way.
        let mut held = self.nonce.lock().await;
        let canonical = serde_json::to_string(&body)?;
        for attempt in 0..2 {
            let nonce = held.clone();
            let message = machine_message(E::PATH, &nonce, &sha256_hex(canonical.as_bytes()));
            let signature = self.key.sign_message(message.as_bytes())?.to_string();
            let req = Signed {
                address: self.address.clone(),
                nonce,
                signature,
                body: body.clone(),
            };
            match self
                .transport
                .post(E::PATH, serde_json::to_value(req)?)
                .await
            {
                Ok(v) => {
                    let answer: Answer<R> = serde_json::from_value(v)?;
                    *held = answer.next_nonce;
                    return Ok(answer.body);
                }
                // A stale nonce: register again (resetting it) and retry once.
                Err(ApiError::Unauthorized) if attempt == 0 && self.reregister.is_some() => {
                    let token = (self.reregister.as_ref().expect("checked"))()?;
                    let body = serde_json::to_value(Registration {
                        address: self.address.clone(),
                        token,
                        chain_id: self.chain_id,
                        results: self.results.clone(),
                    })?;
                    let registered: Registered = serde_json::from_value(
                        self.transport
                            .post(FleetRegister::PATH, body)
                            .await
                            .map_err(|e| anyhow::anyhow!("re-registration refused: {e:?}"))?,
                    )?;
                    *held = registered.nonce;
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!("{} refused", E::PATH)));
                }
            }
        }
        anyhow::bail!("{} refused after re-registering", E::PATH)
    }
}

/// A fresh SubmitKey from the machine's randomness.
pub fn fresh_key() -> anyhow::Result<Key> {
    loop {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        if let Ok(k) = Key::from_bytes(&secret) {
            return Ok(k);
        }
    }
}

impl<T: Transport> Transport for std::rc::Rc<T> {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        (**self).post(path, body).await
    }
}
