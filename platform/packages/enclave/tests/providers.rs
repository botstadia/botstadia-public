//! One provider out of allowance or down never stops the runner
//! (TRUST.md#how-the-enclave-talks-to-the-chain): every read and wait goes on
//! to the next, and what needs two still needs two.

use std::io::{Read, Write};

use botstadia_chain::rpc::{Client, Http};
use botstadia_chain::sign::Key;
use botstadia_chain::{Address, B256, U256};
use botstadia_enclave::anchor::past_close;
use botstadia_enclave::chain_view::{ChainView, TwoProviders};
use botstadia_enclave::submit::{ChainSubmitter, ChainTx};

const CHAIN: u64 = 84532;
const HEAD: u64 = 1_795_370_000;
const FINAL: u64 = 100_000;

/// A provider that answers, or one out of its monthly allowance.
fn provider(working: bool) -> String {
    at_head(working, 0)
}

/// A provider `behind` blocks and seconds behind the others.
fn at_head(working: bool, behind: u64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut seen = Vec::new();
            let mut buf = [0u8; 8192];
            let body = loop {
                let Ok(n) = stream.read(&mut buf) else {
                    break Vec::new();
                };
                if n == 0 {
                    break Vec::new();
                }
                seen.extend_from_slice(&buf[..n]);
                let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&seen[..at]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if seen.len() >= at + 4 + len {
                    break seen[at + 4..at + 4 + len].to_vec();
                }
            };
            let asked: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let result = match asked["method"].as_str().unwrap_or_default() {
                "eth_chainId" => serde_json::json!(format!("{CHAIN:#x}")),
                "eth_getTransactionCount" => serde_json::json!("0x7"),
                "eth_getBalance" => serde_json::json!(if behind > 1_000 { "0x0" } else { "0x64" }),
                "eth_call" => serde_json::json!(format!("0x{:064x}", 1)),
                "eth_getBlockByNumber" => serde_json::json!({
                    "number": format!("{:#x}", FINAL - behind),
                    "hash": format!("{:#x}", B256::repeat_byte(1)),
                    "timestamp": format!("{:#x}", HEAD - behind),
                }),
                _ => serde_json::Value::Null,
            };
            let (status, answer) = if working {
                (
                    "200 OK",
                    serde_json::json!({"jsonrpc": "2.0", "id": asked["id"], "result": result}),
                )
            } else {
                (
                    "429 Too Many Requests",
                    serde_json::json!({"jsonrpc": "2.0", "id": asked["id"], "error": {"code": 429, "message": "Monthly capacity limit exceeded"}}),
                )
            };
            let answer = answer.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    url
}

fn clients(urls: &[String]) -> Vec<Client<Http>> {
    urls.iter()
        .map(|u| Client::new(Http::new(u.clone())))
        .collect()
}

fn submitter(urls: &[String]) -> ChainSubmitter {
    ChainSubmitter {
        providers: clients(urls),
        key: Key::from_bytes(&[7; 32]).unwrap(),
        chain_id: CHAIN,
        results: Address::repeat_byte(5),
        verifier: Address::repeat_byte(6),
        guest_image_id: B256::ZERO,
    }
}

#[tokio::test]
async fn the_first_provider_down_stops_no_anchor_and_no_result() {
    let urls = [provider(false), provider(true), provider(true)];
    let sub = submitter(&urls);
    assert_eq!(sub.next_nonce().await.unwrap(), 7);
    assert!(past_close(&sub, HEAD - 3_600).await.unwrap());
    assert!(!past_close(&sub, HEAD).await.unwrap());
}

#[tokio::test]
async fn what_two_must_answer_takes_any_two() {
    let view = TwoProviders::new(
        clients(&[provider(true), provider(false), provider(true)]),
        Address::repeat_byte(5),
    );
    assert_eq!(view.head_times().await.unwrap(), (HEAD, HEAD));
    assert_eq!(view.balance(Address::ZERO).await.unwrap(), U256::from(100));
    assert!(view.key_active(Address::ZERO).await.unwrap());
}

#[tokio::test]
async fn one_answering_of_several_is_never_two() {
    let urls = [provider(true), provider(false), provider(false)];
    let view = TwoProviders::new(clients(&urls), Address::repeat_byte(5));
    assert!(view.head_times().await.is_err());
    assert!(
        view.finalized().await.is_err(),
        "finality is never one's word"
    );
    assert!(view.key_active(Address::ZERO).await.is_err());
    assert!(
        TwoProviders::connect(&urls, CHAIN, Address::repeat_byte(5))
            .await
            .is_err(),
        "connecting to one would answer for both from then on"
    );
}

#[tokio::test]
async fn a_stuck_provider_holds_nothing_back_and_one_ahead_moves_nothing() {
    let stuck = at_head(true, 5_000);
    let urls = [stuck.clone(), provider(true), at_head(true, 2)];
    let view = TwoProviders::new(clients(&urls), Address::repeat_byte(5));
    // The highest two have both reached: the healthy third is asked.
    assert_eq!(view.finalized().await.unwrap(), FINAL - 2);
    assert_eq!(view.head_times().await.unwrap(), (HEAD - 2, HEAD - 2));

    let urls = [provider(true), at_head(true, 4_000), at_head(true, 4_001)];
    let view = TwoProviders::new(clients(&urls), Address::repeat_byte(5));
    assert_eq!(
        view.finalized().await.unwrap(),
        FINAL - 4_000,
        "one ahead alone"
    );
    assert!(
        !past_close(&submitter(&urls), HEAD - 3_600).await.unwrap(),
        "nor the close"
    );
}

#[tokio::test]
async fn a_stuck_first_provider_never_hides_the_float() {
    let urls = [at_head(true, 5_000), provider(true), provider(true)];
    let view = TwoProviders::new(clients(&urls), Address::repeat_byte(5));
    assert_eq!(view.balance(Address::ZERO).await.unwrap(), U256::from(100));
}
