//! The calls an account wallet's operations batch (MONEY.md#placing-bets):
//! claiming whatever is claimable first, so its money can be used, then
//! approving exactly the amount, the eligibility statement if not yet made,
//! and the bets. A withdrawal claims first too.

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::{SolCall, sol};

use super::Contracts;
use super::op::Call;
use crate::abi::Settlement;

sol! {
    #[allow(missing_docs)]
    function approve(address spender, uint256 amount) returns (bool);
    #[allow(missing_docs)]
    function transfer(address to, uint256 amount) returns (bool);
    #[allow(missing_docs)]
    function allowance(address owner, address spender) returns (uint256);
    #[allow(missing_docs)]
    function balanceOf(address who) returns (uint256);
    #[allow(missing_docs)]
    function nonces(address owner) returns (uint256);
}

fn call(to: Address, data: Vec<u8>) -> Call {
    Call {
        to,
        value: U256::ZERO,
        data: data.into(),
    }
}

pub fn claim(c: &Contracts, sessions: Vec<B256>) -> Call {
    call(
        c.settlement,
        Settlement::claimCall { sessions }.abi_encode(),
    )
}

/// A week's claim carries its bound final's entry, or nothing while no
/// result decides it (a refund).
pub fn claim_weeks(c: &Contracts, weeks: Vec<(B256, Bytes)>) -> Call {
    let (weeks_, entries) = weeks.into_iter().unzip();
    call(
        c.settlement,
        Settlement::claimWeeksCall {
            weeks_,
            finalEntries: entries,
        }
        .abi_encode(),
    )
}

/// The entry whose hash is `id` among one `Posted` event's data: a post
/// carries its entries whole, so a week's claim needs nobody's index.
pub fn posted_entry(data: &[u8], id: B256) -> Option<Bytes> {
    let (_, entries) = <(
        alloy_sol_types::sol_data::FixedBytes<32>,
        alloy_sol_types::sol_data::Array<alloy_sol_types::sol_data::Bytes>,
    ) as alloy_sol_types::SolType>::abi_decode_params(data)
    .ok()?;
    entries
        .into_iter()
        .find(|entry| alloy_primitives::keccak256(entry) == id)
}

/// The most pools one operation claims: the rest wait for the next.
pub const CLAIMS_AT_ONCE: usize = 40;

/// What one pool pays a wallet, as the chain answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimable {
    pub id: B256,
    /// `Some` for a week, with the entry its claim passes.
    pub week: Option<Bytes>,
    pub amount: U256,
}

/// The claims one operation makes first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claims {
    pub sessions: Vec<B256>,
    pub weeks: Vec<(B256, Bytes)>,
    /// What they pay together.
    pub total: U256,
}

impl Claims {
    /// The largest first, as many as one operation takes.
    pub fn from_claimable(list: &[Claimable]) -> Self {
        let mut list: Vec<&Claimable> = list.iter().filter(|x| !x.amount.is_zero()).collect();
        list.sort_by_key(|x| std::cmp::Reverse(x.amount));
        let mut out = Self::default();
        for x in list.into_iter().take(CLAIMS_AT_ONCE) {
            match &x.week {
                Some(entry) => out.weeks.push((x.id, entry.clone())),
                None => out.sessions.push(x.id),
            }
            out.total += x.amount;
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.weeks.is_empty()
    }

    pub fn calls(&self, c: &Contracts) -> Vec<Call> {
        let mut out = Vec::new();
        if !self.sessions.is_empty() {
            out.push(claim(c, self.sessions.clone()));
        }
        if !self.weeks.is_empty() {
            out.push(claim_weeks(c, self.weeks.clone()));
        }
        out
    }
}

pub fn approve(c: &Contracts, spender: Address, amount: U256) -> Call {
    call(c.usdc, approveCall { spender, amount }.abi_encode())
}

/// The paymaster's standing allowance, renewed inside an operation.
pub fn approve_paymaster(c: &Contracts) -> Call {
    approve(c, c.paymaster, U256::from(super::paymaster::ALLOWANCE))
}

/// The eligibility statement, made by the wallet calling the contract
/// itself: no signature, since only the wallet can be the caller.
pub fn make_statement(c: &Contracts) -> Call {
    call(c.settlement, Settlement::makeStatementCall {}.abi_encode())
}

pub fn bets(c: &Contracts, items: Vec<Settlement::BetItem>) -> Call {
    call(c.settlement, Settlement::betManyCall { items }.abi_encode())
}

pub fn transfer(c: &Contracts, to: Address, amount: U256) -> Call {
    call(c.usdc, transferCall { to, amount }.abi_encode())
}

/// A first bet's whole batch: claim, approve exactly the stake, the
/// statement if not yet made, the bets, then the paymaster's allowance
/// renewed.
pub fn bet_batch(
    c: &Contracts,
    claims: Vec<B256>,
    items: Vec<Settlement::BetItem>,
    needs_statement: bool,
    renew_paymaster: bool,
) -> Vec<Call> {
    let total = items.iter().fold(U256::ZERO, |t, i| t + i.amount);
    let mut out = Vec::new();
    if !claims.is_empty() {
        out.push(claim(c, claims));
    }
    out.push(approve(c, c.settlement, total));
    if needs_statement {
        out.push(make_statement(c));
    }
    out.push(bets(c, items));
    if renew_paymaster {
        out.push(approve_paymaster(c));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finals_entry_is_found_in_its_post() {
        use alloy_sol_types::SolEvent;
        let entry = Bytes::from(vec![5u8; 130]);
        let posted = crate::abi::Results::Posted {
            post: 9,
            root: B256::ZERO,
            entries: vec![Bytes::from(vec![1u8; 33]), entry.clone()],
        };
        let data = posted.encode_data();
        let id = alloy_primitives::keccak256(&entry);
        assert_eq!(posted_entry(&data, id), Some(entry));
        assert_eq!(posted_entry(&data, B256::ZERO), None);
    }

    #[test]
    fn one_operation_claims_the_largest_first_weeks_included() {
        let mut list: Vec<Claimable> = (1..=50u64)
            .map(|i| Claimable {
                id: B256::from(U256::from(i)),
                week: None,
                amount: U256::from(i),
            })
            .collect();
        let entry = Bytes::from(vec![0xe1]);
        list.push(Claimable {
            id: B256::repeat_byte(7),
            week: Some(entry.clone()),
            amount: U256::from(1_000u64),
        });
        let claims = Claims::from_claimable(&list);
        assert_eq!(claims.sessions.len(), CLAIMS_AT_ONCE - 1);
        assert_eq!(claims.weeks, vec![(B256::repeat_byte(7), entry)]);
        assert!(!claims.sessions.contains(&B256::from(U256::from(1u64))));
        let sessions: u64 = (12..=50).sum();
        assert_eq!(claims.total, U256::from(1_000 + sessions));
    }
}
