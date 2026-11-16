//! The runner's own transactions (TRUST.md#submitting-results): its key's
//! approval, then anchor and result batches. Sent and confirmed by the
//! enclave itself, across its allowlisted providers.

use alloy_sol_types::SolCall;
use botstadia_chain::abi::Results;
use botstadia_chain::rpc::{BlockTag, Client, Http, Receipt};
use botstadia_chain::sign::Key;
use botstadia_chain::{Address, B256, Bytes, tx};

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
    /// Send one call priced to land and wait for its receipt, trying each
    /// provider in turn.
    pub async fn send(&self, to: Address, data: Vec<u8>, polls: usize) -> anyhow::Result<Receipt> {
        let mut last = anyhow::anyhow!("no provider");
        for p in &self.providers {
            let attempt: anyhow::Result<Receipt> = async {
                let nonce = p.nonce(self.key.address(), BlockTag::Latest).await?;
                let gas = p.estimate_gas(self.key.address(), to, &data).await?;
                let price = p.gas_price().await?.max(1);
                let signed = tx::sign(
                    &self.key,
                    self.chain_id,
                    nonce,
                    gas + gas / 4,
                    tx::Fees {
                        max_fee_per_gas: price * 2,
                        max_priority_fee_per_gas: price,
                    },
                    &tx::Call {
                        to,
                        data: data.clone(),
                        value: Default::default(),
                    },
                )?;
                p.send_raw(&signed.raw).await?;
                for _ in 0..polls {
                    if let Some(r) = p.receipt(signed.hash).await? {
                        anyhow::ensure!(r.succeeded(), "reverted");
                        return Ok(r);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                anyhow::bail!("not included yet")
            }
            .await;
            match attempt {
                Ok(r) => return Ok(r),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl Submitter for ChainSubmitter {
    async fn approve_key(&self, journal: &[u8], seal: &[u8]) -> anyhow::Result<()> {
        let p = &self.providers[0];
        let verifier = p
            .call_typed(
                self.results,
                &Results::verifierIdCall {
                    verifier: self.verifier,
                    guestImageId: self.guest_image_id,
                },
                BlockTag::Latest,
            )
            .await?;
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
        self.providers[0]
            .nonce(self.key.address(), BlockTag::Latest)
            .await
    }

    async fn send_at(
        &self,
        nonce: u64,
        to: Address,
        data: Vec<u8>,
        level: u32,
    ) -> anyhow::Result<B256> {
        let mut last = anyhow::anyhow!("no provider");
        for p in &self.providers {
            let attempt: anyhow::Result<B256> = async {
                let gas = p.estimate_gas(self.key.address(), to, &data).await?;
                let mut price = p.gas_price().await?.max(1) * 2;
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
                        data: data.clone(),
                        value: Default::default(),
                    },
                )?;
                p.send_raw(&signed.raw).await?;
                Ok(signed.hash)
            }
            .await;
            match attempt {
                Ok(h) => return Ok(h),
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
