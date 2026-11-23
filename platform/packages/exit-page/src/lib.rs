//! The exit page (TRUST.md#a-way-out-without-us): a single file that runs on
//! the user's own device, not a site, and moves an account wallet with its
//! recovery key alone.
//!
//! - A deployed wallet claims what's claimable and withdraws, paying gas in
//!   USDC through Circle's Paymaster, through a public bundler and a public
//!   RPC tied to no account of ours. It finds claimable sessions and weeks
//!   by scanning our settlement contract's events through that RPC, and a
//!   week's final's entry in the post that carried it. A claim never
//!   decides the withdrawal: if the scan or the claims fail, or on the
//!   user's say, it withdraws the balance without them.
//! - An undeployed wallet can't pay for its own deploy in USDC, so the page
//!   says how much ETH to send to the recovery key's address, and deploys it
//!   from that key with the saved setup data.
//!
//! - Auto-back's money comes back too: what the standing-orders contract
//!   holds set aside is taken back to the wallet and every order cleared, in
//!   the same operation as the withdrawal.
//!
//! The defaults per chain are baked in at build (`EXIT_PAGE_CHAINS`, from
//! `config.yml`), and the user can replace the RPC and the bundler. The key
//! never leaves the page: it signs here.

use botstadia_chain::abi::{Results, Settlement, StandingOrders};
use botstadia_chain::rpc::{BlockTag, Client, Transport};
use botstadia_chain::sign::Key;
use botstadia_chain::wallet::Contracts;
use botstadia_chain::wallet::bundler::{Bundler, Estimate, Flavor, GasPrice, OpReceipt};
use botstadia_chain::wallet::calls::{self, Claims};
use botstadia_chain::wallet::flow::{self, Bundle};
use botstadia_chain::wallet::money::{self, Money, Pool};
use botstadia_chain::wallet::op::{OwnerSignature, UserOp};
use botstadia_chain::wallet::recovery::{self, Save};
use botstadia_chain::wallet::safe::{self, Setup};
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

/// One range of logs. A public RPC hiccups: it's tried a few times before
/// the scan gives up.
async fn logs(
    rpc: &Client<Fetch>,
    address: Address,
    from: u64,
    to: u64,
    topics: &Value,
) -> anyhow::Result<Vec<Value>> {
    let mut tries = 0;
    loop {
        let r = rpc
            .request(
                "eth_getLogs",
                json!([{
                    "address": format!("{address:#x}"),
                    "fromBlock": format!("0x{from:x}"),
                    "toBlock": format!("0x{to:x}"),
                    "topics": topics,
                }]),
            )
            .await;
        match r {
            // Anything but a list is a failure, never "no logs".
            Ok(Value::Array(list)) => return Ok(list),
            Ok(_) if tries >= 3 => anyhow::bail!("the RPC didn't answer with a list of logs"),
            Ok(_) => {
                tries += 1;
                sleep(1000 * tries).await;
            }
            Err(e) if tries >= 3 => return Err(e),
            Err(_) => {
                tries += 1;
                sleep(1000 * tries).await;
            }
        }
    }
}

/// The blocks one log read takes.
const SCAN: u64 = 10_000;
/// More blocks a second than Base could make: it makes five today.
const FASTEST: u64 = 1_000;

/// A block's time. A public RPC hiccups, so it's asked a few times.
async fn time_of(rpc: &Client<Fetch>, number: u64) -> anyhow::Result<u64> {
    let mut tries = 0;
    loop {
        match rpc.block(BlockTag::Number(number)).await {
            Ok(block) => return Ok(block.timestamp),
            Err(e) if tries >= 3 => return Err(e),
            Err(_) => {
                tries += 1;
                sleep(1000 * tries).await;
            }
        }
    }
}

/// Whether `head` could be real: bounded by the time since the start block,
/// not by a number, since a saved page has to work for as long as claims do.
async fn check_head(rpc: &Client<Fetch>, start: u64, head: u64) -> anyhow::Result<()> {
    let since = ((js_sys::Date::now() / 1000.0) as u64).saturating_sub(time_of(rpc, start).await?);
    // A day's room for a clock that's behind.
    let most = start.saturating_add(since.saturating_add(86_400).saturating_mul(FASTEST));
    anyhow::ensure!(
        (start..=most).contains(&head),
        "that RPC's latest block ({head}) can't be right"
    );
    Ok(())
}

/// The first block at or after `time` (seconds), between `lo` and `hi`.
async fn block_at(rpc: &Client<Fetch>, time: u64, lo: u64, hi: u64) -> anyhow::Result<u64> {
    let (mut lo, mut hi) = (lo, hi);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if time_of(rpc, mid).await? < time {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// A week decided by its final's result, whose claim carries that entry.
struct Decided {
    week: B256,
    final_session: B256,
    pool_close: u64,
    final_close: u64,
}

/// Put each decided week into `out` with its final's entry, read from the
/// post that carried it: made between the week's pool closing and its final
/// closing, so that's all that's searched.
async fn with_entries(
    rpc: &Client<Fetch>,
    ch: &Chain,
    results: Address,
    head: u64,
    decided: Vec<Decided>,
    out: &mut Vec<Pool>,
) -> anyhow::Result<()> {
    use alloy_sol_types::SolEvent;
    let topics = json!([format!("{:#x}", Results::Posted::SIGNATURE_HASH)]);
    // Arenas' weeks share their times.
    let mut spans = std::collections::BTreeMap::<(u64, u64), Vec<Decided>>::new();
    for d in decided {
        spans
            .entry((d.pool_close, d.final_close))
            .or_default()
            .push(d);
    }
    let mut failed = None;
    for (times, left) in spans {
        // One span that can't be read never loses the others.
        if let Err(e) = entries_in(rpc, ch, results, head, &topics, times, left, out).await {
            failed = Some(e);
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// [`with_entries`] for the weeks sharing one pair of times.
#[allow(clippy::too_many_arguments)]
async fn entries_in(
    rpc: &Client<Fetch>,
    ch: &Chain,
    results: Address,
    head: u64,
    topics: &Value,
    (opened, closed): (u64, u64),
    mut left: Vec<Decided>,
    out: &mut Vec<Pool>,
) -> anyhow::Result<()> {
    let first = block_at(rpc, opened, ch.start_block, head).await?;
    let mut to = block_at(rpc, closed, first, head).await?;
    while !left.is_empty() {
        let from = to.saturating_sub(SCAN - 1).max(first);
        for l in logs(rpc, results, from, to, topics).await? {
            let Some(data) = l["data"].as_str().and_then(|d| d.parse::<Bytes>().ok()) else {
                continue;
            };
            left.retain(|d| match calls::posted_entry(&data, d.final_session) {
                Some(entry) => {
                    out.push(Pool {
                        id: d.week,
                        week: Some(entry),
                    });
                    false
                }
                None => true,
            });
        }
        if from <= first {
            break;
        }
        to = from - 1;
    }
    Ok(())
}

/// Where a week stands for its claim: `None` if it's claimed already.
async fn week_pool(
    rpc: &Client<Fetch>,
    ch: &Chain,
    results: Address,
    wallet: Address,
    week: B256,
) -> anyhow::Result<Option<Result<Pool, Decided>>> {
    let settlement = ch.contracts.settlement;
    let call = Settlement::weekClaimedCall {
        _0: week,
        _1: wallet,
    };
    if rpc.call_typed(settlement, &call, BlockTag::Latest).await? {
        return Ok(None);
    }
    let state = rpc
        .call_typed(results, &Results::weekStateCall { week }, BlockTag::Latest)
        .await?;
    if !state.isCancelled && state.finalSession != B256::ZERO {
        let call = Results::sessionStateCall {
            session: state.finalSession,
        };
        let of_final = rpc.call_typed(results, &call, BlockTag::Latest).await?;
        if of_final.resolved && !of_final.voided {
            let times = rpc
                .call_typed(
                    settlement,
                    &Settlement::weekPoolsCall(week),
                    BlockTag::Latest,
                )
                .await?;
            return Ok(Some(Err(Decided {
                week,
                final_session: state.finalSession,
                pool_close: times.poolClose.to::<u64>(),
                final_close: times.finalClose.to::<u64>(),
            })));
        }
    }
    // A refund, or still pending: no result decides it, so no entry.
    Ok(Some(Ok(Pool {
        id: week,
        week: Some(Bytes::new()),
    })))
}

/// Auto-back's money and orders (MONEY.md#standing-orders): what the
/// orders contract holds set aside for the wallet, and every bot whose last
/// order isn't cleared, from its `OrderSet` events.
#[derive(Clone, Debug, Default)]
struct Orders {
    balance: U256,
    bots: Vec<B256>,
    /// Why the orders couldn't be listed: the money still comes back.
    missed: Option<String>,
}

async fn orders_of(rpc: &Client<Fetch>, ch: &Chain, wallet: Address) -> anyhow::Result<Orders> {
    use alloy_sol_types::SolEvent;
    let Some(at) = ch.contracts.standing_orders else {
        return Ok(Orders::default());
    };
    let balance: U256 = match rpc
        .call_typed(at, &StandingOrders::balancesCall(wallet), BlockTag::Latest)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            return Ok(Orders {
                missed: Some(format!(
                    "what's set aside for auto-back couldn't be read: {e:#}"
                )),
                ..Orders::default()
            });
        }
    };
    // Nothing set aside, nothing an order can spend: no scan.
    if balance.is_zero() {
        return Ok(Orders::default());
    }
    let listed = async {
        let head = rpc.block_number().await?;
        let topics = json!([
            format!("{:#x}", StandingOrders::OrderSet::SIGNATURE_HASH),
            format!("{:#x}", B256::left_padding_from(wallet.as_slice())),
        ]);
        let mut live: Vec<B256> = Vec::new();
        let mut from = ch.start_block;
        while from <= head {
            let to = (from + SCAN - 1).min(head);
            for l in logs(rpc, at, from, to, &topics).await? {
                let Some(bot) = l["topics"][2].as_str().and_then(|t| t.parse::<B256>().ok()) else {
                    continue;
                };
                let data = l["data"]
                    .as_str()
                    .and_then(|d| d.parse::<Bytes>().ok())
                    .unwrap_or_default();
                let amount = data.get(..32).map(U256::from_be_slice).unwrap_or_default();
                live.retain(|b| *b != bot);
                if !amount.is_zero() {
                    live.push(bot);
                }
            }
            from = to + 1;
        }
        anyhow::Ok(live)
    };
    Ok(match listed.await {
        Ok(bots) => Orders {
            balance,
            bots,
            missed: None,
        },
        Err(e) => Orders {
            balance,
            bots: Vec::new(),
            missed: Some(format!(
                "auto-back's orders couldn't be listed, so they stay set: {e:#}"
            )),
        },
    })
}

/// What the scan found, and what it couldn't read.
struct Found {
    pools: Vec<Pool>,
    missed: Option<String>,
}

/// The pools the wallet has a stake in, sessions and weeks. A week that
/// can't be read is left out, and never the rest with it.
async fn scan(rpc: &Client<Fetch>, ch: &Chain, wallet: Address) -> anyhow::Result<Found> {
    use alloy_sol_types::SolEvent;
    let settlement = ch.contracts.settlement;
    let head = rpc.block_number().await?;
    check_head(rpc, ch.start_block, head).await?;
    let week_topic = format!("{:#x}", Settlement::WeekBet::SIGNATURE_HASH);
    // Both events name the pool first and the bettor second.
    let topics = json!([
        [
            format!("{:#x}", Settlement::Bet::SIGNATURE_HASH),
            week_topic
        ],
        null,
        format!("{:#x}", B256::left_padding_from(wallet.as_slice())),
    ]);
    let (mut sessions, mut weeks) = (Vec::<B256>::new(), Vec::<B256>::new());
    let mut from = ch.start_block;
    while from <= head {
        let to = (from + SCAN - 1).min(head);
        for l in logs(rpc, settlement, from, to, &topics).await? {
            let list = if l["topics"][0].as_str() == Some(week_topic.as_str()) {
                &mut weeks
            } else {
                &mut sessions
            };
            if let Some(id) = l["topics"][1].as_str().and_then(|t| t.parse::<B256>().ok())
                && !list.contains(&id)
            {
                list.push(id);
            }
        }
        from = to + 1;
    }
    let mut found = Found {
        pools: sessions
            .into_iter()
            .map(|id| Pool { id, week: None })
            .collect(),
        missed: None,
    };
    if weeks.is_empty() {
        return Ok(found);
    }
    let results = match rpc
        .call_typed(settlement, &Settlement::RESULTSCall {}, BlockTag::Latest)
        .await
    {
        Ok(results) => results,
        Err(e) => {
            found.missed = Some(format!("week pools couldn't be read: {e:#}"));
            return Ok(found);
        }
    };
    let mut decided = Vec::new();
    for week in weeks {
        match week_pool(rpc, ch, results, wallet, week).await {
            Ok(Some(Ok(pool))) => found.pools.push(pool),
            Ok(Some(Err(d))) => decided.push(d),
            Ok(None) => {}
            Err(e) => found.missed = Some(format!("a week pool couldn't be read: {e:#}")),
        }
    }
    let wanted = decided.len() + found.pools.len();
    match with_entries(rpc, ch, results, head, decided, &mut found.pools).await {
        Ok(()) if found.pools.len() < wanted => {
            found.missed = Some("a week's final wasn't found in the posts".into());
        }
        Ok(()) => {}
        Err(e) => found.missed = Some(format!("a week's final couldn't be read: {e:#}")),
    }
    Ok(found)
}

/// [`money::read`], asked a few times: one hiccup mustn't waste the scan.
async fn money_read(
    rpc: &Client<Fetch>,
    c: &Contracts,
    wallet: Address,
    pools: &[Pool],
) -> anyhow::Result<Money> {
    let mut tries = 0;
    loop {
        match money::read(rpc, c, wallet, pools).await {
            Ok(now) => return Ok(now),
            Err(e) if tries >= 3 => return Err(e),
            Err(_) => {
                tries += 1;
                sleep(1000 * tries).await;
            }
        }
    }
}

/// The balance and what's claimable, and why any claim was left out. Only
/// the balance itself failing fails this: a claim never decides.
async fn money_of(
    rpc: &Client<Fetch>,
    ch: &Chain,
    wallet: Address,
    claiming: bool,
) -> anyhow::Result<(Money, Option<String>)> {
    let c = &ch.contracts;
    let mut missed = None;
    if claiming {
        match scan(rpc, ch, wallet).await {
            Ok(found) => match money_read(rpc, c, wallet, &found.pools).await {
                Ok(now) => {
                    let unread = (now.unread > 0)
                        .then(|| format!("{} of the pools didn't answer", now.unread));
                    return Ok((now, found.missed.or(unread)));
                }
                Err(e) => missed = Some(format!("claims couldn't be read: {e:#}")),
            },
            Err(e) => missed = Some(format!("the scan for claims failed: {e:#}")),
        }
    }
    let usdc = rpc
        .call_typed(
            c.usdc,
            &calls::balanceOfCall { who: wallet },
            BlockTag::Latest,
        )
        .await?;
    let claimable = Vec::new();
    let now = Money {
        usdc,
        claimable,
        unread: 0,
    };
    Ok((now, missed))
}

/// What the page knows: the wallet, whether it's deployed, its USDC, what's
/// claimable, and for an undeployed one the ETH its deploy needs at the
/// recovery key's address.
#[wasm_bindgen]
pub async fn inspect(
    save_text: String,
    key_text: String,
    rpc_url: String,
    skip_claims: bool,
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
        let (now, missed) = money_of(&rpc, &ch, save.wallet, deployed && !skip_claims).await?;
        let usdc = now.usdc;
        let set_aside = match ch.contracts.standing_orders {
            Some(at) if deployed => rpc
                .call_typed(
                    at,
                    &StandingOrders::balancesCall(save.wallet),
                    BlockTag::Latest,
                )
                .await
                .ok(),
            _ => None,
        };
        let owners = flow::owners(&rpc, c, save.wallet, &save.setup_data).await?;
        let mut v = json!({
            "wallet": format!("{:#x}", save.wallet),
            "chain_id": save.chain_id,
            "deployed": deployed,
            "usdc": usdc.to_string(),
            "set_aside": set_aside.map(|b| b.to_string()),
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
            let list = now.claimable;
            v["claim_missed"] = missed.into();
            v["claimable_total"] = list
                .iter()
                .fold(U256::ZERO, |t, x| t + x.amount)
                .to_string()
                .into();
            v["claimable_next"] = Claims::from_claimable(&list).total.to_string().into();
            v["claimable"] = list
                .iter()
                .map(|x| {
                    json!({
                        "id": format!("{:#x}", x.id),
                        "week": x.week.is_some(),
                        "amount": x.amount.to_string(),
                    })
                })
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
    skip_claims: bool,
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
        let (now, mut missed) = money_of(&rpc, &ch, save.wallet, !skip_claims).await?;
        let orders = orders_of(&rpc, &ch, save.wallet).await?;
        let taken_back = orders.balance;
        let stop = if taken_back.is_zero() && orders.bots.is_empty() {
            Vec::new()
        } else {
            calls::orders_stop(c, orders.bots.clone(), taken_back)?
        };
        if let Some(m) = &orders.missed {
            missed = Some(missed.map_or(m.clone(), |x| format!("{x}; {m}")));
        }
        let claims = Claims::from_claimable(&now.claimable);
        anyhow::ensure!(
            fixed <= now.usdc + claims.total + taken_back,
            "the wallet holds less than that{}",
            missed
                .as_ref()
                .map(|m| format!(" ({m})"))
                .unwrap_or_default()
        );
        // With the claims and auto-back's money, then without the claims,
        // then the wallet's own USDC alone, if those didn't go out and it
        // covers the amount: a stranger filling an order between the read
        // and the send never stops the withdrawal.
        let mut tries = vec![(claims.clone(), true)];
        if !claims.is_empty()
            && fixed <= now.usdc + taken_back
            && !(now.usdc + taken_back).is_zero()
        {
            tries.push((Claims::default(), true));
        }
        if !stop.is_empty() && fixed <= now.usdc && !now.usdc.is_zero() {
            tries.push((Claims::default(), false));
        }
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
        let signer = KeySigner(key);
        let mut last = None;
        for (claims, with_stop) in tries {
            let back = if with_stop { taken_back } else { U256::ZERO };
            let total = now.usdc + claims.total + back;
            let sent = std::cell::Cell::new(U256::ZERO);
            let build = |max_cost: U256| {
                let value = if everything && max_cost == U256::MAX {
                    // The estimate's placeholder: half, which always fits.
                    total / U256::from(2)
                } else if everything {
                    total.saturating_sub(max_cost.max(U256::from(1)))
                } else {
                    fixed
                };
                sent.set(value);
                let mut v = if with_stop { stop.clone() } else { Vec::new() };
                v.extend(claims.calls(c));
                v.push(calls::transfer(c, to, value.max(U256::from(1))));
                v
            };
            match flow::send_built(&rpc, &bundle, &signer, c, save.wallet, build, sleep).await {
                Ok(r) => {
                    if let Some(e) = last.take() {
                        missed = Some(format!(
                            "withdrawn without {}, since with them it failed: {e:#}",
                            if with_stop {
                                "claiming"
                            } else {
                                "claiming or taking back auto-back's money"
                            }
                        ));
                    }
                    return Ok(json!({
                        "tx": format!("{:#x}", r.transaction_hash),
                        "claimed": claims.total.to_string(),
                        "taken_back": back.to_string(),
                        "sent": sent.get().to_string(),
                        "claim_missed": missed,
                    }));
                }
                // It may still land: never a second in its place.
                Err(e) if flow::may_have_landed(&e) => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("nothing was sent")))
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
