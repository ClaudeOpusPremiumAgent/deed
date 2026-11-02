//! Deed: an NEP-171 NFT that owns a keyless NEAR account. Sell the NFT and the money, the revenue
//! and the right to operate the account move with it, in one transaction.
//!
//! The rule that makes this safe under asynchronous execution: every outflow from a vault goes
//! through this contract and bumps the deed's `nonce`, and `pending` counts outflows still running.
//! A buyer names the nonce they inspected. `buy` runs in a single receipt here, so it either sees
//! the exact state the buyer inspected and takes ownership, or it panics and the deposit comes back.
use near_contract_standards::non_fungible_token::core::{NonFungibleTokenCore, NonFungibleTokenResolver};
use near_contract_standards::non_fungible_token::enumeration::NonFungibleTokenEnumeration;
use near_contract_standards::non_fungible_token::events::NftMint;
use near_contract_standards::non_fungible_token::metadata::{
    NFTContractMetadata, NonFungibleTokenMetadataProvider, TokenMetadata, NFT_METADATA_SPEC,
};
use near_contract_standards::non_fungible_token::{NonFungibleToken, Token, TokenId};
use near_sdk::collections::{LazyOption, LookupMap};
use near_sdk::json_types::{Base64VecU8, U128};
use near_sdk::serde_json::json;
use near_sdk::{
    assert_one_yocto, borsh, env, near, require, AccountId, BorshStorageKey, CryptoHash, Gas, NearToken,
    PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};
use std::collections::HashMap;

/// Pays for this contract's per-deed state at its maximum size (about 30 KB: 64 history entries of
/// up to 311 bytes, 20 inflows, 16 call records, metadata), so no deed can eat into another's storage
/// funds. The rest of the mint deposit seeds the vault.
const STATE_COST: NearToken = NearToken::from_millinear(350);
const MIN_VAULT_SEED: NearToken = NearToken::from_millinear(10);
/// Below this a payment still counts as revenue but is not listed, so dust cannot push real
/// customers off the list.
const MIN_LISTED_INFLOW: NearToken = NearToken::from_millinear(1);
const MAX_HISTORY: usize = 64;
const MAX_INFLOWS: usize = 20;
// shortcut: 16 arbitrary calls per deed, raise when a real business needs more
const MAX_CALLS: usize = 16;
const MAX_MEMO: usize = 64;
const MAX_TITLE: usize = 64;
const MAX_DESCRIPTION: usize = 256;

const SEND_GAS: Gas = Gas::from_tgas(5);
// Must also cover loading the largest return value a callee can afford (about 3 MB, since returning
// data costs about 69 Mgas per byte), or it could starve this callback and leave `pending` stuck.
// See the huge_return_value test.
const CALLBACK_GAS: Gas = Gas::from_tgas(30);
const VAULT_OVERHEAD_GAS: Gas = Gas::from_tgas(5);
const TOKEN_TRANSFER_TGAS: u64 = 30;
const MINT_CALLBACK_GAS: Gas = Gas::from_tgas(30);

#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Owners,
    Metadata,
    Enumeration,
    PerOwner,
    ContractMetadata,
    Deeds,
}

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct Operator {
    pub account_id: AccountId,
    pub budget: U128,
    pub expires_at_ms: u64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub enum Kind {
    Mint,
    Pay,
    FtPay,
    NftPay,
    Call,
    SetOperator,
    RevokeOperator,
    List,
    Unlist,
    Sale,
    Transfer,
    TransferReverted,
}

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct Entry {
    /// The deed's nonce when this happened. Identifies an outflow for its callback.
    pub seq: u64,
    pub kind: Kind,
    pub actor: AccountId,
    pub to: Option<AccountId>,
    pub amount: Option<U128>,
    pub token: Option<String>,
    pub memo: Option<String>,
    /// None while running, then whether the outflow succeeded.
    pub ok: Option<bool>,
    pub at_ms: u64,
}

/// An arbitrary call. Kept for the life of the deed: unlike typed payments it can leave rights
/// behind on other contracts, so a buyer must be able to read every one.
#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct CallRecord {
    pub seq: u64,
    pub receiver_id: AccountId,
    pub method: String,
    pub args_sha256: String,
    pub deposit: U128,
    pub ok: Option<bool>,
    pub at_ms: u64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct Inflow {
    pub from: AccountId,
    pub amount: U128,
    pub memo: Option<String>,
    pub at_ms: u64,
}

#[near(serializers = [borsh])]
pub struct DeedState {
    nonce: u64,
    pending: u32,
    epoch: u32,
    price: Option<u128>,
    operator: Option<Operator>,
    paid_out: u128,
    earned: u128,
    payments: u64,
    inflows: Vec<Inflow>,
    calls: Vec<CallRecord>,
    history: Vec<Entry>,
}

#[near(serializers = [json])]
pub struct DeedInfo {
    pub token_id: TokenId,
    pub owner_id: AccountId,
    pub vault_id: AccountId,
    pub nonce: u64,
    pub pending: u32,
    pub epoch: u32,
    pub price: Option<U128>,
    pub operator: Option<Operator>,
    pub paid_out: U128,
    /// Revenue recorded through `pay_in`. Plain transfers to the vault also work but are not counted.
    pub earned: U128,
    pub payments: u64,
    pub inflows: Vec<Inflow>,
    /// True when no arbitrary call was ever made: only typed payments have left this account.
    pub sealed: bool,
    pub calls: Vec<CallRecord>,
    pub history: Vec<Entry>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Deed {
    nft: NonFungibleToken,
    metadata: LazyOption<NFTContractMetadata>,
    vault_code_hash: CryptoHash,
    deeds: LookupMap<TokenId, DeedState>,
}

fn valid_token_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// Limits a memo in bytes, because bytes are what storage costs.
fn clip(memo: Option<String>) -> Option<String> {
    memo.map(|mut m| {
        let mut end = m.len().min(MAX_MEMO);
        while !m.is_char_boundary(end) {
            end -= 1;
        }
        m.truncate(end);
        m
    })
}

fn last_promise_ok() -> bool {
    // The result is not used, so it is not copied out: a too-long result still means success.
    env::promise_results_count() == 1 && !matches!(env::promise_result_checked(0, 0), Err(PromiseError::Failed))
}

/// Borsh arguments for the vault's `call`.
fn vault_call(receiver: &AccountId, method: &str, args: &[u8], deposit: u128, gas_tgas: u64) -> Vec<u8> {
    borsh::to_vec(&(receiver.as_str(), method, args, deposit, gas_tgas)).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn event(name: &str, data: near_sdk::serde_json::Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({ "standard": "deed", "version": "1.0.0", "event": name, "data": data })
    ));
}

impl DeedState {
    fn new() -> Self {
        Self {
            nonce: 0,
            pending: 0,
            epoch: 0,
            price: None,
            operator: None,
            paid_out: 0,
            earned: 0,
            payments: 0,
            inflows: vec![],
            calls: vec![],
            history: vec![],
        }
    }

    fn push(&mut self, entry: Entry) {
        if self.history.len() == MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(entry);
    }

    /// Everything a previous holder could still reach dies here.
    fn change_hands(&mut self) {
        require!(self.pending == 0, "an outflow is still running, try again after it settles");
        self.nonce += 1;
        self.epoch += 1;
        self.price = None;
        self.operator = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn entry(seq: u64, kind: Kind, to: Option<AccountId>, amount: Option<U128>, token: Option<String>, memo: Option<String>, ok: Option<bool>) -> Entry {
    Entry { seq, kind, actor: env::predecessor_account_id(), to, amount, token, memo, ok, at_ms: env::block_timestamp_ms() }
}

#[near]
impl Deed {
    /// Publishes the vault code as an immutable global contract, addressed by hash.
    #[init]
    pub fn new(#[serializer(borsh)] vault_code: Vec<u8>) -> Self {
        // The parent can init in the same transaction that creates the account, so the contract
        // never needs an access key and there is no gap for anyone else to init it first.
        let me = env::current_account_id();
        let caller = env::predecessor_account_id();
        require!(caller == me || me.as_str().ends_with(&format!(".{caller}")), "only this account or its parent");
        let hash: CryptoHash = env::sha256_array(&vault_code).into();
        Promise::new(env::current_account_id()).deploy_global_contract(vault_code).detach();
        Self {
            nft: NonFungibleToken::new(
                StorageKey::Owners,
                env::current_account_id(),
                Some(StorageKey::Metadata),
                Some(StorageKey::Enumeration),
                None::<StorageKey>,
            ),
            metadata: LazyOption::new(
                StorageKey::ContractMetadata,
                Some(&NFTContractMetadata {
                    spec: NFT_METADATA_SPEC.to_string(),
                    name: "Deed".to_string(),
                    symbol: "DEED".to_string(),
                    icon: None,
                    base_uri: None,
                    reference: None,
                    reference_hash: None,
                }),
            ),
            vault_code_hash: hash,
            deeds: LookupMap::new(StorageKey::Deeds),
        }
    }

    /// Creates the keyless vault, then mints. If the name is taken nothing is minted and the
    /// deposit returns. Anything above the state cost seeds the vault.
    #[payable]
    pub fn mint(&mut self, token_id: TokenId, title: String, description: Option<String>, receiver_id: Option<AccountId>) -> Promise {
        require!(valid_token_id(&token_id), "token_id must be 2-32 chars of a-z, 0-9 or -");
        require!(title.len() <= MAX_TITLE, "title too long");
        require!(description.as_ref().map_or(0, |d| d.len()) <= MAX_DESCRIPTION, "description too long");
        let deposit = env::attached_deposit();
        let required = STATE_COST.saturating_add(MIN_VAULT_SEED);
        require!(deposit >= required, format!("attach at least {required}"));
        let payer = env::predecessor_account_id();
        let owner = receiver_id.unwrap_or_else(|| payer.clone());
        let vault = self.vault_of(&token_id);
        Promise::new(vault)
            .create_account()
            .use_global_contract(self.vault_code_hash)
            .transfer(deposit.saturating_sub(STATE_COST))
            .then(Self::ext(env::current_account_id()).with_static_gas(MINT_CALLBACK_GAS).on_vault_created(
                token_id, title, description, owner, payer, U128(deposit.as_yoctonear()),
            ))
    }

    #[private]
    pub fn on_vault_created(
        &mut self,
        token_id: TokenId,
        title: String,
        description: Option<String>,
        owner: AccountId,
        payer: AccountId,
        deposit: U128,
    ) -> bool {
        if !last_promise_ok() {
            Promise::new(payer).transfer(NearToken::from_yoctonear(deposit.0)).detach();
            return false;
        }
        let metadata = TokenMetadata {
            title: Some(title),
            description,
            media: None,
            media_hash: None,
            copies: Some(1),
            issued_at: None,
            expires_at: None,
            starts_at: None,
            updated_at: None,
            extra: None,
            reference: None,
            reference_hash: None,
        };
        // The mint deposit already paid for storage, and a callback has no deposit to refund from.
        self.nft.internal_mint_with_refund(token_id.clone(), owner.clone(), Some(metadata), None);
        NftMint { owner_id: &owner, token_ids: &[&token_id], memo: None }.emit();
        let mut state = DeedState::new();
        state.push(entry(0, Kind::Mint, Some(owner), None, None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
        event("mint", json!({ "token_id": token_id, "vault_id": self.vault_of(&token_id) }));
        true
    }

    /// Revenue: anyone can pay a deed and the money goes to its vault. It does not bump the nonce, so
    /// nobody can block a sale by paying.
    #[payable]
    pub fn pay_in(&mut self, token_id: TokenId, memo: Option<String>) -> Promise {
        let deposit = env::attached_deposit();
        require!(!deposit.is_zero(), "attach NEAR");
        let mut state = self.deeds.get(&token_id).unwrap_or_else(|| env::panic_str("no such deed"));
        state.earned += deposit.as_yoctonear();
        state.payments += 1;
        if deposit >= MIN_LISTED_INFLOW {
            if state.inflows.len() == MAX_INFLOWS {
                state.inflows.remove(0);
            }
            let from = env::predecessor_account_id();
            state.inflows.push(Inflow { from, amount: U128(deposit.as_yoctonear()), memo: clip(memo), at_ms: env::block_timestamp_ms() });
        }
        self.deeds.insert(&token_id, &state);
        Promise::new(self.vault_of(&token_id)).transfer(deposit)
    }

    /// Pays NEAR out of the vault. The owner needs a full-access key because of the 1 yocto. An
    /// operator signs with a function-call key and is held to its budget and expiry.
    #[payable]
    pub fn pay(&mut self, token_id: TokenId, receiver_id: AccountId, amount: U128, memo: Option<String>) -> Promise {
        let actor = env::predecessor_account_id();
        let mut state = self.live_state(&token_id);
        let by_operator = if actor == self.owner_of(&token_id) {
            assert_one_yocto();
            false
        } else {
            let operator = state.operator.as_mut().unwrap_or_else(|| env::panic_str("not the owner or operator"));
            require!(operator.account_id == actor, "not the owner or operator");
            require!(env::attached_deposit().is_zero(), "operators attach nothing");
            require!(env::block_timestamp_ms() < operator.expires_at_ms, "operator expired");
            require!(amount.0 <= operator.budget.0, "over the operator budget");
            operator.budget = U128(operator.budget.0 - amount.0);
            true
        };
        let memo = clip(memo);
        let seq = self.start_outflow(&mut state, Kind::Pay, Some(receiver_id.clone()), Some(amount), None, memo);
        let epoch = state.epoch;
        self.deeds.insert(&token_id, &state);
        let args = borsh::to_vec(&(receiver_id.as_str(), amount.0)).unwrap();
        Promise::new(self.vault_of(&token_id))
            .function_call("send", args, NearToken::from_yoctonear(0), SEND_GAS)
            .then(Self::ext(env::current_account_id()).with_static_gas(CALLBACK_GAS).on_outflow(
                token_id,
                seq,
                Some(amount),
                by_operator.then_some(epoch),
            ))
    }

    /// Pays a NEP-141 token out of the vault. Owner only.
    #[payable]
    pub fn ft_pay(&mut self, token_id: TokenId, token: AccountId, receiver_id: AccountId, amount: U128, memo: Option<String>) -> Promise {
        assert_one_yocto();
        let memo = clip(memo);
        let args = json!({ "receiver_id": receiver_id, "amount": amount, "memo": memo }).to_string().into_bytes();
        self.typed_call(token_id, Kind::FtPay, token, "ft_transfer", args, receiver_id, Some(amount), memo)
    }

    /// Sends an NFT the vault holds. Owner only.
    #[payable]
    pub fn nft_pay(&mut self, token_id: TokenId, nft_contract: AccountId, nft_token_id: String, receiver_id: AccountId) -> Promise {
        assert_one_yocto();
        require!(nft_contract != env::current_account_id(), "a vault cannot hold deeds");
        let args = json!({ "receiver_id": receiver_id, "token_id": nft_token_id }).to_string().into_bytes();
        self.typed_call(token_id, Kind::NftPay, nft_contract, "nft_transfer", args, receiver_id, None, Some(nft_token_id))
    }

    /// Calls any contract from the vault. Owner only. This is the one path that can leave rights
    /// behind on another contract (an approval, a deposit credited elsewhere), so each call is kept
    /// forever and the deed stops being `sealed`.
    #[payable]
    pub fn call(&mut self, token_id: TokenId, receiver_id: AccountId, method: String, args: Base64VecU8, deposit: U128, gas_tgas: u64) -> Promise {
        assert_one_yocto();
        require!(receiver_id != env::current_account_id(), "a vault cannot call the deed contract");
        require!(method.len() <= MAX_MEMO, "method name too long");
        let owner = self.owner_of(&token_id);
        require!(env::predecessor_account_id() == owner, "only the owner");
        let mut state = self.live_state(&token_id);
        require!(state.calls.len() < MAX_CALLS, "this deed has used all its arbitrary calls");
        state.calls.push(CallRecord {
            seq: state.nonce + 1,
            receiver_id: receiver_id.clone(),
            method: method.clone(),
            args_sha256: hex(&env::sha256(&args.0)),
            deposit,
            ok: None,
            at_ms: env::block_timestamp_ms(),
        });
        let seq = self.start_outflow(&mut state, Kind::Call, Some(receiver_id.clone()), Some(deposit), Some(method.clone()), None);
        self.deeds.insert(&token_id, &state);
        let gas = Gas::from_tgas(gas_tgas).saturating_add(VAULT_OVERHEAD_GAS);
        Promise::new(self.vault_of(&token_id))
            .function_call("call", vault_call(&receiver_id, &method, &args.0, deposit.0, gas_tgas), NearToken::from_yoctonear(0), gas)
            .then(Self::ext(env::current_account_id()).with_static_gas(CALLBACK_GAS).on_outflow(token_id, seq, Some(deposit), None))
    }

    /// Runs when an outflow has finished. Reserved gas guarantees it runs, and it does not panic,
    /// so `pending` cannot get stuck.
    #[private]
    pub fn on_outflow(&mut self, token_id: TokenId, seq: u64, amount: Option<U128>, refund_budget_epoch: Option<u32>) -> bool {
        let ok = last_promise_ok();
        let mut state = self.deeds.get(&token_id).unwrap();
        state.pending -= 1;
        if let Some(open) = state.history.iter_mut().rev().find(|e| e.seq == seq) {
            open.ok = Some(ok);
        }
        if let Some(record) = state.calls.iter_mut().find(|c| c.seq == seq) {
            record.ok = Some(ok);
        }
        if ok {
            state.paid_out += amount.map_or(0, |a| a.0);
        } else if refund_budget_epoch == Some(state.epoch) {
            if let (Some(operator), Some(amount)) = (state.operator.as_mut(), amount) {
                operator.budget = U128(operator.budget.0 + amount.0);
            }
        }
        self.deeds.insert(&token_id, &state);
        event("outflow", json!({ "token_id": token_id, "ok": ok, "nonce": state.nonce }));
        ok
    }

    /// Lets an account spend from the vault within a budget, for example an agent running the
    /// business. It cannot call arbitrary contracts and its rights die when the deed changes hands.
    #[payable]
    pub fn set_operator(&mut self, token_id: TokenId, operator_id: AccountId, budget: U128, expires_at_ms: u64) {
        let mut state = self.owner_state(&token_id);
        state.operator = Some(Operator { account_id: operator_id.clone(), budget, expires_at_ms });
        state.nonce += 1;
        state.push(entry(state.nonce, Kind::SetOperator, Some(operator_id), Some(budget), None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
    }

    #[payable]
    pub fn revoke_operator(&mut self, token_id: TokenId) {
        let mut state = self.owner_state(&token_id);
        state.operator = None;
        state.nonce += 1;
        state.push(entry(state.nonce, Kind::RevokeOperator, None, None, None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
    }

    /// Puts the deed up for sale. While listed nothing can leave the vault.
    #[payable]
    pub fn list(&mut self, token_id: TokenId, price: U128) {
        let mut state = self.owner_state(&token_id);
        require!(price.0 > 0, "price must be above zero");
        state.price = Some(price.0);
        state.nonce += 1;
        state.push(entry(state.nonce, Kind::List, None, Some(price), None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
        event("list", json!({ "token_id": token_id, "price": price, "nonce": state.nonce }));
    }

    #[payable]
    pub fn unlist(&mut self, token_id: TokenId) {
        let mut state = self.owner_state(&token_id);
        state.price = None;
        state.nonce += 1;
        state.push(entry(state.nonce, Kind::Unlist, None, None, None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
    }

    /// Buys at the state the buyer inspected. `expected_nonce` pins the price, the operator and
    /// every outflow so far, so anything the seller did since then aborts the sale and returns the
    /// deposit. Revenue arriving meanwhile does not bump the nonce: it only helps the buyer.
    #[payable]
    pub fn buy(&mut self, token_id: TokenId, expected_nonce: u64) {
        let buyer = env::predecessor_account_id();
        let seller = self.owner_of(&token_id);
        let mut state = self.deeds.get(&token_id).unwrap_or_else(|| env::panic_str("no such deed"));
        let price = state.price.unwrap_or_else(|| env::panic_str("not for sale"));
        require!(buyer != seller, "you already own this deed");
        require!(!self.is_vault(&buyer), "a vault cannot own a deed");
        require!(state.nonce == expected_nonce, "the deed changed since you inspected it");
        let deposit = env::attached_deposit().as_yoctonear();
        require!(deposit >= price, "attach the full price");
        state.change_hands();
        state.push(entry(state.nonce, Kind::Sale, Some(seller.clone()), Some(U128(price)), None, None, Some(true)));
        self.deeds.insert(&token_id, &state);
        self.nft.internal_transfer(&seller, &buyer, &token_id, None, None);
        Promise::new(seller.clone()).transfer(NearToken::from_yoctonear(price)).detach();
        if deposit > price {
            Promise::new(buyer.clone()).transfer(NearToken::from_yoctonear(deposit - price)).detach();
        }
        event("sale", json!({ "token_id": token_id, "seller": seller, "buyer": buyer, "price": U128(price), "nonce": state.nonce }));
    }

    pub fn deed_info(&self, token_id: TokenId) -> Option<DeedInfo> {
        let owner_id = self.nft.owner_by_id.get(&token_id)?;
        let state = self.deeds.get(&token_id)?;
        Some(DeedInfo {
            vault_id: self.vault_of(&token_id),
            token_id,
            owner_id,
            nonce: state.nonce,
            pending: state.pending,
            epoch: state.epoch,
            price: state.price.map(U128),
            operator: state.operator,
            paid_out: U128(state.paid_out),
            earned: U128(state.earned),
            payments: state.payments,
            inflows: state.inflows,
            sealed: state.calls.is_empty(),
            calls: state.calls,
            history: state.history,
        })
    }

    pub fn vault_code_hash(&self) -> String {
        hex(&self.vault_code_hash)
    }
}

impl Deed {
    fn vault_of(&self, token_id: &str) -> AccountId {
        format!("{token_id}.{}", env::current_account_id()).parse().unwrap_or_else(|_| env::panic_str("invalid vault name"))
    }

    fn is_vault(&self, account: &AccountId) -> bool {
        account.as_str().ends_with(&format!(".{}", env::current_account_id()))
    }

    fn owner_of(&self, token_id: &TokenId) -> AccountId {
        self.nft.owner_by_id.get(token_id).unwrap_or_else(|| env::panic_str("no such deed"))
    }

    /// State for an action that needs the deed not to be for sale.
    fn live_state(&self, token_id: &TokenId) -> DeedState {
        let state = self.deeds.get(token_id).unwrap_or_else(|| env::panic_str("no such deed"));
        require!(state.price.is_none(), "the deed is listed: unlist it first");
        state
    }

    fn owner_state(&self, token_id: &TokenId) -> DeedState {
        assert_one_yocto();
        require!(env::predecessor_account_id() == self.owner_of(token_id), "only the owner");
        self.deeds.get(token_id).unwrap_or_else(|| env::panic_str("no such deed"))
    }

    #[allow(clippy::too_many_arguments)]
    fn start_outflow(&self, state: &mut DeedState, kind: Kind, to: Option<AccountId>, amount: Option<U128>, token: Option<String>, memo: Option<String>) -> u64 {
        state.nonce += 1;
        state.pending += 1;
        state.push(entry(state.nonce, kind, to, amount, token, memo, None));
        state.nonce
    }

    #[allow(clippy::too_many_arguments)]
    fn typed_call(&mut self, token_id: TokenId, kind: Kind, contract: AccountId, method: &str, args: Vec<u8>, to: AccountId, amount: Option<U128>, memo: Option<String>) -> Promise {
        require!(env::predecessor_account_id() == self.owner_of(&token_id), "only the owner");
        let mut state = self.live_state(&token_id);
        let seq = self.start_outflow(&mut state, kind, Some(to), amount, Some(contract.to_string()), clip(memo));
        self.deeds.insert(&token_id, &state);
        let gas = Gas::from_tgas(TOKEN_TRANSFER_TGAS).saturating_add(VAULT_OVERHEAD_GAS);
        Promise::new(self.vault_of(&token_id))
            .function_call("call", vault_call(&contract, method, &args, 1, TOKEN_TRANSFER_TGAS), NearToken::from_yoctonear(0), gas)
            .then(Self::ext(env::current_account_id()).with_static_gas(CALLBACK_GAS).on_outflow(token_id, seq, None, None))
    }

    fn before_transfer(&mut self, token_id: &TokenId, receiver_id: &AccountId, kind: Kind) {
        require!(!self.is_vault(receiver_id), "a vault cannot own a deed");
        let mut state = self.deeds.get(token_id).unwrap_or_else(|| env::panic_str("no such deed"));
        state.change_hands();
        state.push(entry(state.nonce, kind, Some(receiver_id.clone()), None, None, None, Some(true)));
        self.deeds.insert(token_id, &state);
    }
}

#[near]
impl NonFungibleTokenCore for Deed {
    #[payable]
    fn nft_transfer(&mut self, receiver_id: AccountId, token_id: TokenId, approval_id: Option<u64>, memo: Option<String>) {
        self.before_transfer(&token_id, &receiver_id, Kind::Transfer);
        self.nft.nft_transfer(receiver_id, token_id, approval_id, memo);
    }

    #[payable]
    fn nft_transfer_call(&mut self, receiver_id: AccountId, token_id: TokenId, approval_id: Option<u64>, memo: Option<String>, msg: String) -> PromiseOrValue<bool> {
        self.before_transfer(&token_id, &receiver_id, Kind::Transfer);
        self.nft.nft_transfer_call(receiver_id, token_id, approval_id, memo, msg)
    }

    fn nft_token(&self, token_id: TokenId) -> Option<Token> {
        self.nft.nft_token(token_id)
    }
}

#[near]
impl NonFungibleTokenResolver for Deed {
    #[private]
    fn nft_resolve_transfer(
        &mut self,
        previous_owner_id: AccountId,
        receiver_id: AccountId,
        token_id: TokenId,
        approved_account_ids: Option<HashMap<AccountId, u64>>,
    ) -> bool {
        let transferred = self.nft.nft_resolve_transfer(previous_owner_id.clone(), receiver_id, token_id.clone(), approved_account_ids);
        if !transferred {
            if let Some(mut state) = self.deeds.get(&token_id) {
                state.nonce += 1;
                state.epoch += 1;
                state.push(entry(state.nonce, Kind::TransferReverted, Some(previous_owner_id), None, None, None, Some(true)));
                self.deeds.insert(&token_id, &state);
            }
        }
        transferred
    }
}

#[near]
impl NonFungibleTokenEnumeration for Deed {
    fn nft_total_supply(&self) -> U128 {
        self.nft.nft_total_supply()
    }

    fn nft_tokens(&self, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.nft.nft_tokens(from_index, limit)
    }

    fn nft_supply_for_owner(&self, account_id: AccountId) -> U128 {
        self.nft.nft_supply_for_owner(account_id)
    }

    fn nft_tokens_for_owner(&self, account_id: AccountId, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.nft.nft_tokens_for_owner(account_id, from_index, limit)
    }
}

#[near]
impl NonFungibleTokenMetadataProvider for Deed {
    fn nft_metadata(&self) -> NFTContractMetadata {
        self.metadata.get().unwrap()
    }
}
