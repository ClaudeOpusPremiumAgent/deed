//! Test helper: a call that takes several blocks to finish, a method that fails, one that returns a
//! huge value, and just enough of NEP-141, NEP-171 and the NFT receiver to exercise a deed's inventory.
use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, serde_json::json, AccountId, Gas, NearToken, Promise, PromiseOrValue};
use std::collections::HashMap;

static BIG: [u8; 4_194_304] = [0; 4_194_304];

#[near(contract_state)]
#[derive(Default)]
pub struct Slow {
    balances: HashMap<AccountId, u128>,
    nft_owner: Option<AccountId>,
}

#[near]
impl Slow {
    #[payable]
    pub fn spin(&mut self, hops: u32) -> PromiseOrValue<u32> {
        if hops == 0 {
            return PromiseOrValue::Value(0);
        }
        let args = json!({ "hops": hops - 1 }).to_string().into_bytes();
        let gas = env::prepaid_gas().saturating_sub(Gas::from_tgas(3));
        PromiseOrValue::Promise(Promise::new(env::current_account_id()).function_call("spin", args, NearToken::from_near(0), gas))
    }

    #[payable]
    pub fn fail(&mut self) {
        env::panic_str("slow: deliberate failure");
    }

    pub fn big(&self, bytes: u32) {
        // A static buffer: filling a Vec byte by byte would burn the gas before anything is returned.
        env::value_return(&BIG[..bytes as usize]);
    }

    pub fn ft_mint(&mut self, account_id: AccountId, amount: U128) {
        *self.balances.entry(account_id).or_default() += amount.0;
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balances.get(&account_id).copied().unwrap_or(0))
    }

    #[payable]
    pub fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        assert_one_yocto();
        let _ = memo;
        let sender = env::predecessor_account_id();
        let balance = self.balances.entry(sender).or_default();
        assert!(*balance >= amount.0, "not enough tokens");
        *balance -= amount.0;
        *self.balances.entry(receiver_id).or_default() += amount.0;
    }

    pub fn nft_give(&mut self, account_id: AccountId) {
        self.nft_owner = Some(account_id);
    }

    pub fn nft_holder(&self) -> Option<AccountId> {
        self.nft_owner.clone()
    }

    #[payable]
    pub fn nft_transfer(&mut self, receiver_id: AccountId, token_id: String) {
        assert_one_yocto();
        let _ = token_id;
        assert_eq!(self.nft_owner.as_ref(), Some(&env::predecessor_account_id()), "not the holder");
        self.nft_owner = Some(receiver_id);
    }

    /// NFT receiver: `msg` of "return" sends the token back.
    pub fn nft_on_transfer(&mut self, sender_id: AccountId, previous_owner_id: AccountId, token_id: String, msg: String) -> bool {
        let _ = (sender_id, previous_owner_id, token_id);
        msg == "return"
    }
}
