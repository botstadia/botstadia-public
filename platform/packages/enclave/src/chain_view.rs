//! What the runner reads from the chain (TRUST.md#how-the-enclave-talks-to-
//! the-chain): contract getters at a finalized block, acted on only when two
//! allowlisted providers agree. Never through the api.

use botstadia_chain::abi::Results;
use botstadia_chain::rpc::{BlockTag, Client, Http};
use botstadia_chain::{Address, B256, U256};

#[allow(async_fn_in_trait)]
pub trait ChainView {
    /// The newest new-key instruction in effect, with the BundleKey the
    /// chain holds for it (zero until filled).
    async fn newest_instruction(&self) -> anyhow::Result<Option<(u64, B256)>>;
    /// The BundleKey the chain holds for an instruction (zero if unfilled).
    async fn held_key(&self, instruction: u64) -> anyhow::Result<B256>;
    /// An image digest approved and neither retired nor revoked.
    async fn image_usable(&self, digest: B256) -> anyhow::Result<bool>;
    /// A GoogleRootCert hash approved and neither retired nor revoked.
    async fn root_usable(&self, root: B256) -> anyhow::Result<bool>;
    /// A SubmitKey approved and not revoked by any route.
    async fn key_active(&self, key: Address) -> anyhow::Result<bool>;
    /// An address's ETH (latest: a float is spent on inclusion).
    async fn balance(&self, address: Address) -> anyhow::Result<U256>;
    /// A post's root and block time (zero root if no such post).
    async fn post(&self, number: u64) -> anyhow::Result<(B256, u64)>;
    /// Whether a bundle digest has a final rating, or was named a seed bot.
    async fn rated_or_seed(&self, digest: B256) -> anyhow::Result<bool>;
    /// The same, as of the block that recorded post `post`
    /// (TRUST.md#only-rated-bots-play-bettable-sessions).
    async fn rated_or_seed_by_post(&self, digest: B256, post: u64) -> anyhow::Result<bool>;
    /// The latest and the finalized blocks' times: the finality lag.
    async fn head_times(&self) -> anyhow::Result<(u64, u64)>;
    /// Whether a bundle digest was named a seed bot (seated from the CDN).
    async fn seed_bot(&self, digest: B256) -> anyhow::Result<bool>;
    /// The finalized block's number.
    async fn finalized(&self) -> anyhow::Result<u64>;
    /// A ladder's count of rated and seed bots, which the contract keeps.
    async fn ladder_count(&self, ladder: B256) -> anyhow::Result<u64>;
    /// A session's accepted result, if any: whether it voided, its first
    /// places (bit i: the spec's entrant i) and its archive hash.
    async fn result(&self, session: B256) -> anyhow::Result<Option<OnChainResult>>;
    /// The SubmitKey a session is bound to (zero if unanchored).
    async fn bound_key(&self, session: B256) -> anyhow::Result<Address>;
    /// Whether a week is called off, and the final bound to it (zero if none).
    async fn week_state(&self, week: B256) -> anyhow::Result<(bool, B256)>;
    /// A week's field entry hash and the post that first listed it (zero
    /// hash if none): the one field entry the week runs on.
    async fn week_field(&self, week: B256) -> anyhow::Result<(B256, u64)>;
    /// How many posts there are.
    async fn post_count(&self) -> anyhow::Result<u64>;
}

/// A session's accepted result, as the results contract keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnChainResult {
    pub voided: bool,
    pub first_mask: U256,
    pub archive_hash: B256,
}

/// The allowlisted providers. Locally both are Anvil; in an image they're
/// pinned hostnames, each checked to serve the binding's chain id.
pub struct TwoProviders {
    pub providers: Vec<Client<Http>>,
    pub results: Address,
}

impl TwoProviders {
    pub async fn connect(urls: &[String], chain_id: u64, results: Address) -> anyhow::Result<Self> {
        let mut providers = Vec::new();
        for url in urls {
            let c = Client::new(Http::new(url.clone()));
            // A provider counts only once it serves the binding's chain.
            if c.chain_id().await.ok() == Some(chain_id) {
                providers.push(c);
            }
        }
        anyhow::ensure!(
            !providers.is_empty(),
            "no allowlisted provider serves chain {chain_id}"
        );
        Ok(Self { providers, results })
    }

    /// The two answers that must agree (one provider answers twice when only
    /// one is configured, as locally).
    fn pair(&self) -> (&Client<Http>, &Client<Http>) {
        (
            &self.providers[0],
            self.providers.get(1).unwrap_or(&self.providers[0]),
        )
    }

    async fn agreed<C>(&self, call: &C) -> anyhow::Result<C::Return>
    where
        C: alloy_sol_types::SolCall,
        C::Return: PartialEq + std::fmt::Debug,
    {
        let block = self.finalized_block().await?;
        self.agreed_at(call, block).await
    }

    /// The finalized block, the lower of the first two providers' that
    /// answer (or one's, when only one is configured).
    async fn finalized_block(&self) -> anyhow::Result<u64> {
        let mut seen = Vec::with_capacity(2);
        let mut last_error = None;
        for p in &self.providers {
            match p.block(BlockTag::SETTLED).await {
                Ok(b) => seen.push(b.number),
                Err(e) => last_error = Some(e),
            }
            if seen.len() == 2 {
                break;
            }
        }
        seen.into_iter()
            .min()
            .ok_or_else(|| last_error.unwrap_or_else(|| anyhow::anyhow!("no provider answered")))
    }

    /// A getter two providers agree on, at `block`: the first two that
    /// answer, so one provider out of allowance or down never stops a read
    /// (one provider answers twice when only one is configured, as locally).
    async fn agreed_at<C>(&self, call: &C, block: u64) -> anyhow::Result<C::Return>
    where
        C: alloy_sol_types::SolCall,
        C::Return: PartialEq + std::fmt::Debug,
    {
        let mut answers = Vec::with_capacity(2);
        let mut last_error = None;
        for p in &self.providers {
            match p
                .call_typed(self.results, call, BlockTag::Number(block))
                .await
            {
                Ok(v) => answers.push(v),
                Err(e) => last_error = Some(e),
            }
            if answers.len() == 2 {
                break;
            }
        }
        if self.providers.len() == 1 && answers.len() == 1 {
            let again = self.providers[0]
                .call_typed(self.results, call, BlockTag::Number(block))
                .await?;
            answers.push(again);
        }
        match (answers.pop(), answers.pop()) {
            (Some(y), Some(x)) => {
                anyhow::ensure!(x == y, "the providers disagree");
                Ok(x)
            }
            _ => Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no two providers answered"))),
        }
    }

    /// The post count at `block`, agreed: zero before the contract existed,
    /// where a call answers with no data at all (found on the testnet: the
    /// bisection below asked blocks from before the deploy, and every
    /// session's hold failed decoding the empty answer).
    async fn post_count_at(&self, block: u64) -> anyhow::Result<u64> {
        use alloy_sol_types::SolCall;
        let data = Results::postCountCall {}.abi_encode();
        let mut answers = Vec::with_capacity(2);
        let mut last_error = None;
        for p in &self.providers {
            match p.call(self.results, &data, BlockTag::Number(block)).await {
                Ok(v) => answers.push(v),
                Err(e) => last_error = Some(e),
            }
            if answers.len() == 2 {
                break;
            }
        }
        if self.providers.len() == 1 && answers.len() == 1 {
            answers.push(answers[0].clone());
        }
        let (Some(y), Some(x)) = (answers.pop(), answers.pop()) else {
            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no two providers answered")));
        };
        anyhow::ensure!(x == y, "the providers disagree");
        post_count_of(&x)
    }

    /// The block that recorded post `number`: the first whose post count
    /// reaches it, found by bisection at finalized blocks.
    async fn post_block(&self, number: u64) -> anyhow::Result<u64> {
        let mut hi = self.finalized_block().await?;
        anyhow::ensure!(
            self.post_count_at(hi).await? >= number,
            "post {number} isn't final"
        );
        let mut lo = 0u64;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.post_count_at(mid).await? >= number {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Ok(lo)
    }
}

impl ChainView for TwoProviders {
    async fn newest_instruction(&self) -> anyhow::Result<Option<(u64, B256)>> {
        let count = self.agreed(&Results::instructionCountCall {}).await?;
        for n in (1..=count).rev() {
            if self
                .agreed(&Results::instructionInEffectCall { number: n })
                .await?
            {
                return Ok(Some((n, self.held_key(n).await?)));
            }
        }
        Ok(None)
    }

    async fn held_key(&self, instruction: u64) -> anyhow::Result<B256> {
        Ok(self
            .agreed(&Results::instructionsCall(instruction))
            .await?
            .bundleKey)
    }

    async fn image_usable(&self, digest: B256) -> anyhow::Result<bool> {
        let gate = self.agreed(&Results::imagesCall(digest)).await?;
        Ok(self.agreed(&Results::imageApprovedCall { digest }).await?
            && !gate.retired
            && !gate.revoked)
    }

    async fn root_usable(&self, root: B256) -> anyhow::Result<bool> {
        let gate = self.agreed(&Results::rootsCall(root)).await?;
        Ok(self
            .agreed(&Results::rootApprovedCall { rootHash: root })
            .await?
            && !gate.retired
            && !gate.revoked)
    }

    async fn key_active(&self, key: Address) -> anyhow::Result<bool> {
        // Approval acts on inclusion: the key needs nothing final to start.
        let (a, b) = self.pair();
        let x = a
            .call_typed(
                self.results,
                &Results::keyActiveCall { key },
                BlockTag::Latest,
            )
            .await?;
        let y = b
            .call_typed(
                self.results,
                &Results::keyActiveCall { key },
                BlockTag::Latest,
            )
            .await?;
        Ok(x && y)
    }

    async fn balance(&self, address: Address) -> anyhow::Result<U256> {
        let (a, b) = self.pair();
        Ok(a.balance(address, BlockTag::Latest)
            .await?
            .min(b.balance(address, BlockTag::Latest).await?))
    }

    async fn post(&self, number: u64) -> anyhow::Result<(B256, u64)> {
        let p = self.agreed(&Results::postsCall(number)).await?;
        Ok((p.root, p.time))
    }

    async fn rated_or_seed(&self, digest: B256) -> anyhow::Result<bool> {
        Ok(self.agreed(&Results::ratingsCall(digest)).await?.set || self.seed_bot(digest).await?)
    }

    async fn seed_bot(&self, digest: B256) -> anyhow::Result<bool> {
        self.agreed(&Results::seedBotsCall(digest)).await
    }

    async fn rated_or_seed_by_post(&self, digest: B256, post: u64) -> anyhow::Result<bool> {
        let block = self.post_block(post).await?;
        Ok(self
            .agreed_at(&Results::ratingsCall(digest), block)
            .await?
            .set
            || self
                .agreed_at(&Results::seedBotsCall(digest), block)
                .await?)
    }

    async fn head_times(&self) -> anyhow::Result<(u64, u64)> {
        let (a, b) = self.pair();
        let latest = a
            .block(BlockTag::Latest)
            .await?
            .timestamp
            .min(b.block(BlockTag::Latest).await?.timestamp);
        let finalized = a
            .block(BlockTag::SETTLED)
            .await?
            .timestamp
            .min(b.block(BlockTag::SETTLED).await?.timestamp);
        Ok((latest, finalized))
    }

    async fn finalized(&self) -> anyhow::Result<u64> {
        self.finalized_block().await
    }

    async fn ladder_count(&self, ladder: B256) -> anyhow::Result<u64> {
        Ok(self
            .agreed(&Results::ladderCountCall(ladder))
            .await?
            .try_into()
            .unwrap_or(u64::MAX))
    }

    async fn result(&self, session: B256) -> anyhow::Result<Option<OnChainResult>> {
        let r = self.agreed(&Results::resultsCall(session)).await?;
        Ok(r.accepted.then(|| OnChainResult {
            voided: r.voided,
            first_mask: U256::from(r.firstMask),
            archive_hash: r.archiveHash,
        }))
    }

    async fn bound_key(&self, session: B256) -> anyhow::Result<Address> {
        Ok(self.agreed(&Results::anchorsCall(session)).await?.key)
    }

    async fn week_state(&self, week: B256) -> anyhow::Result<(bool, B256)> {
        let w = self.agreed(&Results::weekStateCall { week }).await?;
        Ok((w.isCancelled, w.finalSession))
    }

    async fn week_field(&self, week: B256) -> anyhow::Result<(B256, u64)> {
        let f = self.agreed(&Results::weekFieldsCall(week)).await?;
        Ok((f.entryHash, f.post))
    }

    async fn post_count(&self) -> anyhow::Result<u64> {
        self.agreed(&Results::postCountCall {}).await
    }
}

/// A chain in memory, for tests (feature `testing`).
#[cfg(feature = "fake-chain")]
#[derive(Default)]
pub struct FakeChain {
    /// (number, held key) of the newest instruction in effect.
    pub instruction: std::cell::Cell<Option<(u64, B256)>>,
    pub approved: std::cell::RefCell<std::collections::HashSet<Address>>,
    pub balances: std::cell::RefCell<std::collections::HashMap<Address, U256>>,
    pub images_ok: std::cell::Cell<bool>,
    pub posts: std::cell::RefCell<std::collections::HashMap<u64, (B256, u64)>>,
    pub rated: std::cell::RefCell<std::collections::HashSet<B256>>,
    /// The post from which a rated bot counts (absent: since forever).
    pub rated_from: std::cell::RefCell<std::collections::HashMap<B256, u64>>,
    /// The latest and finalized blocks' times.
    pub head_times: std::cell::Cell<(u64, u64)>,
    pub seeds: std::cell::RefCell<std::collections::HashSet<B256>>,
    pub finalized: std::cell::Cell<u64>,
    pub ladder_count: std::cell::Cell<u64>,
    pub results: std::cell::RefCell<std::collections::HashMap<B256, OnChainResult>>,
    pub bound: std::cell::RefCell<std::collections::HashMap<B256, Address>>,
    pub weeks: std::cell::RefCell<std::collections::HashMap<B256, (bool, B256)>>,
    pub week_fields: std::cell::RefCell<std::collections::HashMap<B256, (B256, u64)>>,
}

#[cfg(feature = "fake-chain")]
impl FakeChain {
    pub fn genesis() -> Self {
        let f = Self::default();
        f.instruction.set(Some((1, B256::ZERO)));
        f.images_ok.set(true);
        f
    }
}

#[cfg(feature = "fake-chain")]
impl ChainView for FakeChain {
    async fn newest_instruction(&self) -> anyhow::Result<Option<(u64, B256)>> {
        Ok(self.instruction.get())
    }
    async fn held_key(&self, instruction: u64) -> anyhow::Result<B256> {
        Ok(match self.instruction.get() {
            Some((n, k)) if n == instruction => k,
            _ => B256::ZERO,
        })
    }
    async fn image_usable(&self, _digest: B256) -> anyhow::Result<bool> {
        Ok(self.images_ok.get())
    }
    async fn root_usable(&self, _root: B256) -> anyhow::Result<bool> {
        Ok(self.images_ok.get())
    }
    async fn key_active(&self, key: Address) -> anyhow::Result<bool> {
        Ok(self.approved.borrow().contains(&key))
    }
    async fn balance(&self, address: Address) -> anyhow::Result<U256> {
        Ok(self
            .balances
            .borrow()
            .get(&address)
            .copied()
            .unwrap_or_default())
    }
    async fn post(&self, number: u64) -> anyhow::Result<(B256, u64)> {
        Ok(self
            .posts
            .borrow()
            .get(&number)
            .copied()
            .unwrap_or_default())
    }
    async fn rated_or_seed(&self, digest: B256) -> anyhow::Result<bool> {
        Ok(self.rated.borrow().contains(&digest) || self.seeds.borrow().contains(&digest))
    }
    async fn seed_bot(&self, digest: B256) -> anyhow::Result<bool> {
        Ok(self.seeds.borrow().contains(&digest))
    }
    async fn rated_or_seed_by_post(&self, digest: B256, post: u64) -> anyhow::Result<bool> {
        let from = self.rated_from.borrow().get(&digest).copied().unwrap_or(0);
        Ok(self.rated_or_seed(digest).await? && from <= post)
    }
    async fn head_times(&self) -> anyhow::Result<(u64, u64)> {
        Ok(self.head_times.get())
    }
    async fn finalized(&self) -> anyhow::Result<u64> {
        Ok(self.finalized.get())
    }
    async fn ladder_count(&self, _ladder: B256) -> anyhow::Result<u64> {
        Ok(self.ladder_count.get())
    }
    async fn result(&self, session: B256) -> anyhow::Result<Option<OnChainResult>> {
        Ok(self.results.borrow().get(&session).copied())
    }
    async fn bound_key(&self, session: B256) -> anyhow::Result<Address> {
        Ok(self
            .bound
            .borrow()
            .get(&session)
            .copied()
            .unwrap_or_default())
    }
    async fn week_state(&self, week: B256) -> anyhow::Result<(bool, B256)> {
        Ok(self.weeks.borrow().get(&week).copied().unwrap_or_default())
    }
    async fn week_field(&self, week: B256) -> anyhow::Result<(B256, u64)> {
        Ok(self
            .week_fields
            .borrow()
            .get(&week)
            .copied()
            .unwrap_or_default())
    }
    async fn post_count(&self) -> anyhow::Result<u64> {
        Ok(self.posts.borrow().keys().copied().max().unwrap_or(0))
    }
}

/// A `postCount()` answer: no data is no contract yet, so no posts.
fn post_count_of(answer: &[u8]) -> anyhow::Result<u64> {
    use alloy_sol_types::SolCall;
    if answer.is_empty() {
        return Ok(0);
    }
    Ok(Results::postCountCall::abi_decode_returns(answer)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_contract_yet_is_no_posts() {
        assert_eq!(super::post_count_of(&[]).unwrap(), 0);
        let mut seven = [0u8; 32];
        seven[31] = 7;
        assert_eq!(super::post_count_of(&seven).unwrap(), 7);
        assert!(super::post_count_of(&[1, 2, 3]).is_err());
    }
}
