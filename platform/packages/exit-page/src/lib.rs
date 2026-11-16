//! The exit page (TRUST.md#a-way-out-without-us): a single file that runs on
//! the user's own device, not a site, and moves an account wallet with its
//! recovery key alone.
//!
//! - A deployed wallet claims what's claimable and withdraws, paying gas in
//!   USDC through Circle's Paymaster, through a public bundler and a public
//!   RPC tied to no account of ours. It finds claimable sessions by scanning
//!   our settlement contract's events through that RPC.
//! - An undeployed wallet can't pay for its own deploy in USDC, so the page
//!   says how much ETH to send to the recovery key's address, and deploys it
//!   from that key with the saved setup data.
//!
//! The defaults per chain are baked in at build (`EXIT_PAGE_CHAINS`, from
//! `config.yml`), and the user can replace the RPC and the bundler. The key
//! never leaves the page: it signs here.

use botstadia_chain::abi::Settlement;
use botstadia_chain::rpc::{BlockTag, Client, Transport};
use botstadia_chain::sign::Key;
use botstadia_chain::wallet::bundler::{Bundler, Estimate, Flavor, GasPrice, OpReceipt};
use botstadia_chain::wallet::flow::{self, Bundle};
use botstadia_chain::wallet::op::{OwnerSignature, UserOp};
use botstadia_chain::wallet::recovery::{self, Save};
use botstadia_chain::wallet::safe::{self, Setup};
use botstadia_chain::wallet::{Contracts, calls};
use botstadia_chain::{Address, B256, Bytes, U256, keccak256, tx};
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// One chain's defaults.
#[derive(Clone, Debug, Deserialize)]
struct Chain {
    contracts: Contracts,
    /// Our settlement contract's deploy block: the scan starts there.
    start_block: u64,
    rpc: String,
    bundlers: Vec<String>,
}

fn chains() -> anyhow::Result<std::collections::BTreeMap<String, Chain>> {
    Ok(serde_json::from_str(
        option_env!("EXIT_PAGE_CHAINS").unwrap_or("{}"),
    )?)
}

fn chain(id: u64) -> anyhow::Result<Chain> {
    chains()?
        .remove(&id.to_string())
        .ok_or_else(|| anyhow::anyhow!("this page doesn't know chain {id}"))
}

/// JSON-RPC over the page's own fetch.
struct Fetch(String);

impl Transport for Fetch {
    async fn post(&self, body: String) -> anyhow::Result<String> {
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&JsValue::from_str(&body));
        let headers = web_sys::Headers::new().map_err(js)?;
        headers
            .set("content-type", "application/json")
            .map_err(js)?;
        init.set_headers(&headers);
        let req = web_sys::Request::new_with_str_and_init(&self.0, &init).map_err(js)?;
        let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("no window"))?;
        let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&req))
            .await
            .map_err(js)?
            .dyn_into()
            .map_err(js)?;
        let text = JsFuture::from(resp.text().map_err(js)?).await.map_err(js)?;
        anyhow::ensure!(resp.ok(), "{} answered {}", self.0, resp.status());
        text.as_string().ok_or_else(|| anyhow::anyhow!("not text"))
    }
}

fn js(v: JsValue) -> anyhow::Error {
    anyhow::anyhow!("{}", v.as_string().unwrap_or_else(|| format!("{v:?}")))
}

async fn sleep(ms: u32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// A public bundler, priced from the RPC when it has no fee method.
struct PublicBundle {
    bundler: Bundler<Fetch>,
    rpc: Client<Fetch>,
}

impl Bundle for PublicBundle {
    async fn gas_price(&self) -> anyhow::Result<GasPrice> {
        if self.bundler.flavor == Flavor::Pimlico
            && let Ok(p) = self.bundler.gas_price().await
        {
            return Ok(p);
        }
        let tip = U256::from(self.rpc.max_priority_fee().await?.max(1_000_000));
        let base = U256::from(self.rpc.gas_price().await?);
        Ok(GasPrice {
            max_fee_per_gas: base * U256::from(2) + tip,
            max_priority_fee_per_gas: tip,
        })
    }
    async fn estimate(&self, c: &Contracts, op: &UserOp) -> anyhow::Result<Estimate> {
        self.bundler.estimate(c, op).await
    }
    async fn send(&self, c: &Contracts, op: &UserOp) -> anyhow::Result<B256> {
        self.bundler.send(c, op).await
    }
    async fn receipt(&self, hash: B256) -> anyhow::Result<Option<OpReceipt>> {
        self.bundler.receipt(hash).await
    }
    async fn erc20_paymaster(
        &self,
        c: &Contracts,
        op: &UserOp,
        stub: bool,
    ) -> anyhow::Result<Option<flow::PaymasterFields>> {
        Bundle::erc20_paymaster(&self.bundler, c, op, stub).await
    }
}

struct KeySigner(Key);

impl flow::Signer for KeySigner {
    fn passkey(&self) -> bool {
        false
    }
    async fn sign(&self, digest: B256) -> anyhow::Result<OwnerSignature> {
        Ok(OwnerSignature::Address {
            owner: self.0.address(),
            rsv: Bytes::from(self.0.sign_hash(digest)?.as_bytes().to_vec()),
        })
    }
}

fn key_of(save: &Save, typed: &str) -> anyhow::Result<Key> {
    let text = if typed.trim().is_empty() {
        save.recovery_key.clone().unwrap_or_default()
    } else {
        typed.to_string()
    };
    recovery::decode(&text).map_err(|r| match r {
        recovery::Refused::BadChecksum => {
            anyhow::anyhow!("a character of the recovery key is wrong: check your copy")
        }
        recovery::Refused::NotAKey => {
            anyhow::anyhow!("that isn't a recovery key (it starts bsrk1)")
        }
    })
}

fn out(r: anyhow::Result<Value>) -> Result<String, JsValue> {
    r.map(|v| v.to_string())
        .map_err(|e| JsValue::from_str(&format!("{e:#}")))
}

/// The defaults this page carries, per chain.
#[wasm_bindgen]
pub fn defaults() -> Result<String, JsValue> {
    out(chains().map(|c| {
        json!(
            c.iter()
                .map(|(id, ch)| (
                    id.clone(),
                    json!({ "rpc": ch.rpc, "bundlers": ch.bundlers })
                ))
                .collect::<serde_json::Map<_, _>>()
        )
    }))
}

/// The wallet's sessions with a stake, and what each would pay now.
async fn claimable(
    rpc: &Client<Fetch>,
    ch: &Chain,
    wallet: Address,
) -> anyhow::Result<Vec<(B256, U256)>> {
    use alloy_sol_types::SolEvent;
    let head = rpc.block_number().await?;
    let topic_bettor = B256::left_padding_from(wallet.as_slice());
    let mut sessions: Vec<B256> = Vec::new();
    let mut from = ch.start_block;
    while from <= head {
        let to = (from + 9_999).min(head);
        // A public RPC hiccups: each range is tried a few times before the
        // scan gives up.
        let mut tries = 0;
        let logs = loop {
            let r = rpc
                .request(
                    "eth_getLogs",
                    json!([{
                        "address": format!("{:#x}", ch.contracts.settlement),
                        "fromBlock": format!("0x{from:x}"),
                        "toBlock": format!("0x{to:x}"),
                        "topics": [format!("{:#x}", Settlement::Bet::SIGNATURE_HASH), null, format!("{topic_bettor:#x}")],
                    }]),
                )
                .await;
            match r {
                Ok(v) => break v,
                Err(e) if tries >= 3 => return Err(e),
                Err(_) => {
                    tries += 1;
                    sleep(1000 * tries).await;
                }
            }
        };
        for l in logs.as_array().into_iter().flatten() {
            if let Some(s) = l["topics"][1].as_str().and_then(|t| t.parse::<B256>().ok())
                && !sessions.contains(&s)
            {
                sessions.push(s);
            }
        }
        from = to + 1;
    }
    let mut out = Vec::new();
    for s in sessions {
        let r = rpc
            .call_typed(
                ch.contracts.settlement,
                &Settlement::claimableCall {
                    session: s,
                    bettor: wallet,
                },
                BlockTag::Latest,
            )
            .await?;
        if r.amount > U256::ZERO {
            out.push((s, r.amount));
        }
    }
    Ok(out)
}

/// What the page knows: the wallet, whether it's deployed, its USDC, what's
/// claimable, and for an undeployed one the ETH its deploy needs at the
/// recovery key's address.
#[wasm_bindgen]
pub async fn inspect(
    save_text: String,
    key_text: String,
    rpc_url: String,
) -> Result<String, JsValue> {
    out(async {
        let save = Save::parse(&save_text)?;
        let ch = chain(save.chain_id)?;
        let c = &ch.contracts;
        Setup::check_canonical(c, save.wallet, &save.setup_data).map_err(|_| {
            anyhow::anyhow!("the setup data doesn't deploy to that wallet's address")
        })?;
        let key = key_of(&save, &key_text).ok();
        let rpc = Client::new(Fetch(if rpc_url.is_empty() {
            ch.rpc.clone()
        } else {
            rpc_url
        }));
        anyhow::ensure!(
            rpc.chain_id().await? == save.chain_id,
            "that RPC serves another chain"
        );
        let code = rpc
            .request(
                "eth_getCode",
                json!([format!("{:#x}", save.wallet), "latest"]),
            )
            .await?;
        let deployed = code.as_str().is_some_and(|s| s.len() > 2);
        let usdc = rpc
            .call_typed(
                c.usdc,
                &calls::balanceOfCall { who: save.wallet },
                BlockTag::Latest,
            )
            .await?;
        let owners = flow::owners(&rpc, c, save.wallet, &save.setup_data).await?;
        let mut v = json!({
            "wallet": format!("{:#x}", save.wallet),
            "chain_id": save.chain_id,
            "deployed": deployed,
            "usdc": usdc.to_string(),
            "owners": owners.iter().map(|o| format!("{o:#x}")).collect::<Vec<_>>(),
        });
        if let Some(k) = &key {
            v["key_address"] = format!("{:#x}", k.address()).into();
            v["key_is_owner"] = owners.contains(&k.address()).into();
            v["key_eth"] = rpc
                .balance(k.address(), BlockTag::Latest)
                .await?
                .to_string()
                .into();
        }
        if deployed {
            let list = claimable(&rpc, &ch, save.wallet).await?;
            v["claimable_total"] = list
                .iter()
                .fold(U256::ZERO, |t, (_, a)| t + *a)
                .to_string()
                .into();
            v["claimable"] = list
                .iter()
                .map(|(s, a)| json!({ "session": format!("{s:#x}"), "amount": a.to_string() }))
                .collect::<Vec<_>>()
                .into();
        } else if let Some(k) = &key {
            // The deploy's cost at today's price, with room for a rise.
            let data = safe::deploy_call(c, &save.setup_data);
            let gas = rpc
                .estimate_gas(k.address(), c.proxy_factory, &data)
                .await
                .unwrap_or(450_000);
            let price = rpc.gas_price().await?;
            let need = U256::from(gas) * U256::from(price) * U256::from(2);
            v["deploy_eth_needed"] = need.to_string().into();
        }
        Ok(v)
    }
    .await)
}

/// Deploy an undeployed wallet from the recovery key, with its saved setup
/// data: the key pays the gas in ETH.
#[wasm_bindgen]
pub async fn deploy(
    save_text: String,
    key_text: String,
    rpc_url: String,
) -> Result<String, JsValue> {
    out(async {
        let save = Save::parse(&save_text)?;
        let ch = chain(save.chain_id)?;
        let c = &ch.contracts;
        Setup::check_canonical(c, save.wallet, &save.setup_data)?;
        let key = key_of(&save, &key_text)?;
        let rpc = Client::new(Fetch(if rpc_url.is_empty() {
            ch.rpc.clone()
        } else {
            rpc_url
        }));
        let data = safe::deploy_call(c, &save.setup_data);
        let from = key.address();
        let nonce = rpc.nonce(from, BlockTag::Latest).await?;
        let gas = rpc.estimate_gas(from, c.proxy_factory, &data).await?;
        let price = rpc.gas_price().await?.max(1);
        let signed = tx::sign(
            &key,
            save.chain_id,
            nonce,
            gas + gas / 5,
            tx::Fees {
                max_fee_per_gas: price * 2,
                max_priority_fee_per_gas: price.min(price * 2),
            },
            &tx::Call {
                to: c.proxy_factory,
                data,
                value: U256::ZERO,
            },
        )?;
        rpc.send_raw(&signed.raw).await?;
        for _ in 0..120 {
            if let Some(r) = rpc.receipt(signed.hash).await? {
                anyhow::ensure!(r.succeeded(), "the deploy reverted");
                return Ok(json!({ "tx": format!("{:#x}", signed.hash) }));
            }
            sleep(1000).await;
        }
        anyhow::bail!("the deploy {} wasn't included in two minutes", signed.hash)
    }
    .await)
}

/// Claim whatever is claimable and withdraw `amount` USDC (or everything,
/// less what the gas could take, if `amount` is empty) to `to`, in one
/// operation signed by the recovery key, gas paid in USDC.
#[wasm_bindgen]
pub async fn withdraw(
    save_text: String,
    key_text: String,
    to: String,
    amount: String,
    rpc_url: String,
    bundler_url: String,
) -> Result<String, JsValue> {
    out(async {
        let save = Save::parse(&save_text)?;
        let ch = chain(save.chain_id)?;
        let c = &ch.contracts;
        let key = key_of(&save, &key_text)?;
        let to: Address = to
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("that isn't a Base address"))?;
        let rpc = Client::new(Fetch(if rpc_url.is_empty() {
            ch.rpc.clone()
        } else {
            rpc_url.clone()
        }));
        let owners = flow::owners(&rpc, c, save.wallet, &save.setup_data).await?;
        anyhow::ensure!(
            owners.contains(&key.address()),
            "that recovery key isn't an owner of this wallet any more"
        );
        let list = claimable(&rpc, &ch, save.wallet).await?;
        let usdc = rpc
            .call_typed(
                c.usdc,
                &calls::balanceOfCall { who: save.wallet },
                BlockTag::Latest,
            )
            .await?;
        let claim_total = list.iter().fold(U256::ZERO, |t, (_, a)| t + *a);
        // Everything means all but the paymaster's deposit, which it takes
        // before the calls run and refunds after: sized once the gas is known.
        let everything = amount.trim().is_empty();
        let fixed = if everything {
            U256::ZERO
        } else {
            let v: f64 = amount
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("an amount in USDC"))?;
            U256::from((v * 1e6).round() as u64)
        };
        anyhow::ensure!(everything || fixed > U256::ZERO, "nothing to withdraw");
        let claims: Vec<B256> = list.iter().map(|(s, _)| *s).collect();
        let sent = std::cell::Cell::new(U256::ZERO);
        let build = |max_cost: U256| {
            let value = if everything && max_cost == U256::MAX {
                // The estimate's placeholder: half, which always fits.
                (usdc + claim_total) / U256::from(2)
            } else if everything {
                (usdc + claim_total).saturating_sub(max_cost.max(U256::from(1)))
            } else {
                fixed
            };
            sent.set(value);
            let mut v = Vec::new();
            if !claims.is_empty() {
                v.push(calls::claim(c, claims.clone()));
            }
            v.push(calls::transfer(c, to, value.max(U256::from(1))));
            v
        };
        let url = if bundler_url.is_empty() {
            ch.bundlers.first().cloned().unwrap_or_default()
        } else {
            bundler_url
        };
        let flavor = if url.contains("pimlico") {
            Flavor::Pimlico
        } else {
            Flavor::Generic
        };
        let bundle = PublicBundle {
            bundler: Bundler::new(url.clone(), flavor, Fetch(url)),
            rpc: Client::new(Fetch(if rpc_url.is_empty() {
                ch.rpc.clone()
            } else {
                rpc_url
            })),
        };
        let r =
            flow::send_built(&rpc, &bundle, &KeySigner(key), c, save.wallet, build, sleep).await?;
        Ok(json!({
            "tx": format!("{:#x}", r.transaction_hash),
            "claimed": claim_total.to_string(),
            "sent": sent.get().to_string(),
        }))
    }
    .await)
}

/// The save's own check, before anything is asked of the network.
#[wasm_bindgen]
pub fn read_save(save_text: String, key_text: String) -> Result<String, JsValue> {
    out((|| {
        let save = Save::parse(&save_text)?;
        let key = key_of(&save, &key_text).ok();
        Ok(json!({
            "wallet": format!("{:#x}", save.wallet),
            "chain_id": save.chain_id,
            "has_key": key.is_some(),
            "key_address": key.map(|k| format!("{:#x}", k.address())),
            "fingerprint": format!("{:#x}", keccak256(save.setup_data.as_ref())),
        }))
    })())
}
