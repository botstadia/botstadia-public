//! The runner's own transactions (TRUST.md#submitting-results): its key's
//! approval, then anchor and result batches. Sent and confirmed by the
//! enclave itself, across its allowlisted providers.

use alloy_sol_types::SolCall;
use botstadia_chain::abi::Results;
use botstadia_chain::rpc::{BlockTag, Client, Http, Receipt};
use botstadia_chain::sign::Key;
use botstadia_chain::{Address, B256, Bytes, tx};

use crate::chain_view::first_answer;

#[allow(async_fn_in_trait)]
pub trait Submitter {
    /// Send `approveKey` with the published proof, from the key itself.
    async fn approve_key(&self, journal: &[u8], seal: &[u8]) -> anyhow::Result<()>;
}

pub struct ChainSubmitter {
    pub providers: Vec<Client<Http>>,
    pub key: Key,
    pub chain_id: u64,
    pub results: Address,
    /// The verifier and guest image the boot proof is for (constants of the
    /// image; the local deploy's mock verifier locally).
    pub verifier: Address,
    pub guest_image_id: B256,
}

impl ChainSubmitter {
    /// Send one call priced to land and wait for its receipt. A receipt that
    /// can't be read is asked again: the send may well have landed.
    pub async fn send(&self, to: Address, data: Vec<u8>, polls: usize) -> anyhow::Result<Receipt> {
        let nonce = self.next_nonce().await?;
        let hash = self.send_at(nonce, to, data, 0).await?;
        for _ in 0..polls {
            if let Some(r) = self.receipt(hash).await? {
                anyhow::ensure!(r.succeeded(), "reverted");
                return Ok(r);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        anyhow::bail!("not included yet")
    }
}

impl Submitter for ChainSubmitter {
    async fn approve_key(&self, journal: &[u8], seal: &[u8]) -> anyhow::Result<()> {
        let call = Results::verifierIdCall {
            verifier: self.verifier,
            guestImageId: self.guest_image_id,
        };
        let verifier = first_answer!(self.providers, p => p
            .call_typed(self.results, &call, BlockTag::Latest)
            .await)?;
        let data = Results::approveKeyCall {
            verifier,
            seal: Bytes::from(seal.to_vec()),
            journal: Bytes::from(journal.to_vec()),
        }
        .abi_encode();
        self.send(self.results, data, 100).await?;
        Ok(())
    }
}

/// One transaction slot, for batches replaced at the same nonce.
#[allow(async_fn_in_trait)]
pub trait ChainTx {
    async fn next_nonce(&self) -> anyhow::Result<u64>;
    /// Sign and send at `nonce`, at `level` (0 priced to land; each level up
    /// raises the fee a quarter). Returns the hash.
    async fn send_at(
        &self,
        nonce: u64,
        to: Address,
        data: Vec<u8>,
        level: u32,
    ) -> anyhow::Result<B256>;
    async fn receipt(&self, hash: B256) -> anyhow::Result<Option<Receipt>>;
}

impl ChainTx for ChainSubmitter {
    async fn next_nonce(&self) -> anyhow::Result<u64> {
        // The higher of two: one a block behind would reuse a spent nonce.
        let mut seen = Vec::with_capacity(2);
        let mut last = anyhow::anyhow!("no provider");
        for p in &self.providers {
            match p.nonce(self.key.address(), BlockTag::Latest).await {
                Ok(n) => seen.push(n),
                Err(e) => last = e,
            }
            if seen.len() == 2 {
                break;
            }
        }
        seen.into_iter().max().ok_or(last)
    }

    async fn send_at(
        &self,
        nonce: u64,
        to: Address,
        data: Vec<u8>,
        level: u32,
    ) -> anyhow::Result<B256> {
        // The higher of two: one behind simulates the cheap rejection path,
        // and the real anchor runs out of gas.
        let mut estimates = Vec::with_capacity(2);
        let mut last = anyhow::anyhow!("no provider");
        for p in &self.providers {
            match p.estimate_gas(self.key.address(), to, &data).await {
                Ok(gas) => estimates.push(gas),
                Err(e) => last = e,
            }
            if estimates.len() == 2 {
                break;
            }
        }
        let gas = estimates.into_iter().max().ok_or(last)?;
        let mut price = first_answer!(self.providers, p => p.gas_price().await)?.max(1) * 2;
        for _ in 0..level {
            price += price / 4 + 1;
        }
        let signed = tx::sign(
            &self.key,
            self.chain_id,
            nonce,
            gas + gas / 4,
            tx::Fees {
                max_fee_per_gas: price,
                max_priority_fee_per_gas: price / 2,
            },
            &tx::Call {
                to,
                data,
                value: Default::default(),
            },
        )?;
        // The same signed bytes to each: one that forwarded them and then
        // timed out leaves the next saying it knows them.
        let mut last = anyhow::anyhow!("no provider");
        for p in &self.providers {
            match p.send_raw(&signed.raw).await {
                Ok(_) => return Ok(signed.hash),
                Err(e) if format!("{e:#}").contains("already known") => return Ok(signed.hash),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn receipt(&self, hash: B256) -> anyhow::Result<Option<Receipt>> {
        for p in &self.providers {
            if let Ok(Some(r)) = p.receipt(hash).await {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }
}
