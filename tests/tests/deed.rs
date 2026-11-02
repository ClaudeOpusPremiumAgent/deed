//! End-to-end tests on a local NEAR sandbox. Each test states the property it protects.
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};
use std::time::Duration;

const YOCTO: NearToken = NearToken::from_yoctonear(1);

fn near(n: u128) -> NearToken {
    NearToken::from_near(n)
}

fn s(n: u128) -> String {
    NearToken::from_near(n).as_yoctonear().to_string()
}

struct T {
    worker: Worker<Sandbox>,
    deed: Contract,
    slow: Contract,
    alice: Account,
    bob: Account,
    agent: Account,
    carol: Account,
}

async fn setup() -> anyhow::Result<T> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let account = |name: &'static str, n: u128| {
        let root = root.clone();
        async move { anyhow::Ok(root.create_subaccount(name).initial_balance(near(n)).transact().await?.into_result()?) }
    };
    let deed_account = account("deed", 100).await?;
    let deed = deed_account.deploy(&std::fs::read("../target/near/deed_contract/deed_contract.wasm")?).await?.into_result()?;
    let vault_code = std::fs::read("../target/near/deed_vault/deed_vault.wasm")?;
    deed.call("new").args_borsh(vault_code).gas(Gas::from_tgas(300)).transact().await?.into_result()?;
    let slow_account = account("slow", 10).await?;
    let slow = slow_account.deploy(&std::fs::read("../target/near/deed_slow/deed_slow.wasm")?).await?.into_result()?;
    Ok(T {
        deed,
        slow,
        alice: account("alice", 100).await?,
        bob: account("bob", 100).await?,
        agent: account("agent", 10).await?,
        carol: account("carol", 10).await?,
        worker,
    })
}

impl T {
    async fn mint(&self, id: &str, deposit: NearToken) -> anyhow::Result<ExecutionFinalResult> {
        Ok(self.alice.call(self.deed.id(), "mint").args_json(json!({"token_id": id, "title": "Shop"})).deposit(deposit).gas(Gas::from_tgas(100)).transact().await?)
    }

    async fn info(&self, id: &str) -> anyhow::Result<Value> {
        Ok(self.deed.view("deed_info").args_json(json!({"token_id": id})).await?.json()?)
    }

    fn vault(&self, id: &str) -> near_workspaces::AccountId {
        format!("{id}.{}", self.deed.id()).parse().unwrap()
    }

    async fn balance(&self, id: &near_workspaces::AccountId) -> anyhow::Result<u128> {
        Ok(self.worker.view_account(id).await?.balance.as_yoctonear())
    }

    /// Funds a vault the way a customer would.
    async fn revenue(&self, id: &str, amount: NearToken) -> anyhow::Result<()> {
        self.carol.call(self.deed.id(), "pay_in").args_json(json!({"token_id": id, "memo": "order"})).deposit(amount).gas(Gas::from_tgas(50)).transact().await?.into_result()?;
        Ok(())
    }

    async fn pay(&self, who: &Account, id: &str, to: &near_workspaces::AccountId, amount: &str, deposit: NearToken) -> anyhow::Result<ExecutionFinalResult> {
        Ok(who
            .call(self.deed.id(), "pay")
            .args_json(json!({"token_id": id, "receiver_id": to, "amount": amount}))
            .deposit(deposit)
            .gas(Gas::from_tgas(100))
            .transact()
            .await?)
    }

    async fn list(&self, who: &Account, id: &str, price: &str) -> anyhow::Result<ExecutionFinalResult> {
        Ok(who.call(self.deed.id(), "list").args_json(json!({"token_id": id, "price": price})).deposit(YOCTO).transact().await?)
    }

    async fn buy(&self, who: &Account, id: &str, nonce: u64, deposit: NearToken) -> anyhow::Result<ExecutionFinalResult> {
        Ok(who.call(self.deed.id(), "buy").args_json(json!({"token_id": id, "expected_nonce": nonce})).deposit(deposit).gas(Gas::from_tgas(100)).transact().await?)
    }

    async fn call_from_vault(&self, id: &str, method: &str, args: &str) -> anyhow::Result<ExecutionFinalResult> {
        use near_sdk::base64::Engine;
        let args = near_sdk::base64::engine::general_purpose::STANDARD.encode(args);
        Ok(self
            .alice
            .call(self.deed.id(), "call")
            .args_json(json!({"token_id": id, "receiver_id": self.slow.id(), "method": method, "args": args, "deposit": "0", "gas_tgas": 220}))
            .deposit(YOCTO)
            .gas(Gas::from_tgas(300))
            .transact()
            .await?)
    }
}

fn failed_with(result: &ExecutionFinalResult, needle: &str) {
    assert!(result.is_failure(), "expected a failure mentioning {needle:?}");
    let message = format!("{:?}", result.clone().into_result().unwrap_err());
    assert!(message.contains(needle), "expected {needle:?} in {message}");
}

fn nonce(info: &Value) -> u64 {
    info["nonce"].as_u64().unwrap()
}

/// The vault is a real account nobody can sign for, and a failed mint costs nothing but gas.
#[tokio::test]
async fn mint_creates_a_keyless_vault() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    let info = t.info("shop").await?;
    assert_eq!(info["owner_id"], t.alice.id().as_str());
    assert_eq!(info["vault_id"], t.vault("shop").as_str());
    assert_eq!(info["sealed"], true);
    assert!(t.worker.view_access_keys(&t.vault("shop")).await?.is_empty(), "a vault must have no keys");
    assert_eq!(t.balance(&t.vault("shop")).await?, NearToken::from_millinear(650).as_yoctonear());

    // Unused gas comes back in its own receipts, after the transaction result is already final,
    // so let the refunds of both mints land before comparing balances.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let before = t.balance(t.alice.id()).await?;
    let _ = t.mint("shop", near(1)).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let change = before.abs_diff(t.balance(t.alice.id()).await?);
    assert!(change < NearToken::from_millinear(50).as_yoctonear(), "deposit must come back, changed by {change}");
    let supply: String = t.deed.view("nft_total_supply").await?.json()?;
    assert_eq!(supply, "1");

    let stolen_init = t.bob.call(t.deed.id(), "new").args_borsh(vec![0u8]).gas(Gas::from_tgas(100)).transact().await?;
    assert!(stolen_init.is_failure(), "nobody else can initialise the contract");

    failed_with(&t.mint("Bad.Name", near(1)).await?, "token_id must be");
    failed_with(&t.mint("cheap", NearToken::from_millinear(100)).await?, "attach at least");
    Ok(())
}

/// The vault's power is limited by what it imports, not by promises in its code.
#[test]
fn the_vault_code_cannot_add_keys_deploy_or_delete() -> anyhow::Result<()> {
    let wasm = std::fs::read("../target/near/deed_vault/deed_vault.wasm")?;
    let has = |needle: &str| wasm.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(has("promise_batch_action_transfer"), "sanity check: imports are visible as text");
    for forbidden in ["add_key", "delete_key", "delete_account", "deploy", "create_account", "storage_write"] {
        assert!(!has(forbidden), "the vault must not import anything with {forbidden}");
    }
    Ok(())
}

/// Revenue accrues in the vault, and only the owner can move it.
#[tokio::test]
async fn revenue_and_owner_payments() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    t.revenue("shop", near(2)).await?;
    let long = "ż".repeat(100);
    t.pay(&t.alice, "shop", t.carol.id(), "1", YOCTO).await?.into_result()?;
    t.alice
        .call(t.deed.id(), "pay")
        .args_json(json!({"token_id": "shop", "receiver_id": t.carol.id(), "amount": "1", "memo": long}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?
        .into_result()?;
    let memo = t.info("shop").await?["history"].as_array().unwrap().last().unwrap()["memo"].as_str().unwrap().to_string();
    assert!(memo.len() <= 64, "memos are limited in bytes, got {}", memo.len());
    t.carol.transfer_near(&t.vault("shop"), near(1)).await?.into_result()?;
    let info = t.info("shop").await?;
    assert_eq!(info["earned"], s(2), "a plain transfer is balance, not recorded revenue");
    assert_eq!(info["inflows"].as_array().unwrap().len(), 1);
    assert_eq!(info["inflows"][0]["from"], t.carol.id().as_str());
    assert_eq!(t.balance(&t.vault("shop")).await?, NearToken::from_millinear(3650).as_yoctonear() - 2, "the two 1-yocto memo payments above left the vault");

    failed_with(&t.pay(&t.bob, "shop", t.bob.id(), &s(1), YOCTO).await?, "not the owner or operator");
    failed_with(&t.pay(&t.alice, "shop", t.bob.id(), &s(1), NearToken::from_yoctonear(0)).await?, "1 yoctoNEAR");

    let before = t.balance(t.carol.id()).await?;
    t.pay(&t.alice, "shop", t.carol.id(), &s(1), YOCTO).await?.into_result()?;
    assert_eq!(t.balance(t.carol.id()).await? - before, near(1).as_yoctonear());
    let info = t.info("shop").await?;
    assert_eq!(info["pending"], 0);
    assert_eq!(info["nonce"], 3);
    assert_eq!(info["paid_out"].as_str().unwrap().parse::<u128>()?, near(1).as_yoctonear() + 2);
    assert_eq!(info["history"].as_array().unwrap().last().unwrap()["ok"], true);

    // The vault accepts tokens from anywhere and keeps them.
    let kept: String = t.bob.call(&t.vault("shop"), "ft_on_transfer").args_json(json!({"sender_id": t.bob.id(), "amount": "999", "msg": ""})).transact().await?.json()?;
    assert_eq!(kept, "0");

    // Only the deed contract can instruct a vault, whatever the arguments.
    for method in ["send", "call"] {
        failed_with(&t.bob.call(&t.vault("shop"), method).args(vec![1, 2, 3]).transact().await?, "only the deed contract");
    }
    Ok(())
}

/// An agent can run the business on a budget without holding the deed.
#[tokio::test]
async fn operator_is_held_to_budget_and_expiry() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(5)).await?.into_result()?;
    let far = 4_000_000_000_000u64;
    let set = |expiry: u64| {
        t.alice
            .call(t.deed.id(), "set_operator")
            .args_json(json!({"token_id": "shop", "operator_id": t.agent.id(), "budget": s(1), "expires_at_ms": expiry}))
            .deposit(YOCTO)
            .transact()
    };
    set(far).await?.into_result()?;
    let none = NearToken::from_yoctonear(0);
    let part = NearToken::from_millinear(600).as_yoctonear().to_string();
    t.pay(&t.agent, "shop", t.carol.id(), &part, none).await?.into_result()?;
    failed_with(&t.pay(&t.agent, "shop", t.carol.id(), &part, none).await?, "over the operator budget");

    // A failed payment gives the budget back and leaves the money in the vault.
    let vault_before = t.balance(&t.vault("shop")).await?;
    let ghost: near_workspaces::AccountId = "ghost-does-not-exist.test.near".parse()?;
    let small = NearToken::from_millinear(100).as_yoctonear().to_string();
    let _ = t.pay(&t.agent, "shop", &ghost, &small, none).await?;
    let info = t.info("shop").await?;
    assert_eq!(info["operator"]["budget"], NearToken::from_millinear(400).as_yoctonear().to_string());
    assert_eq!(info["history"].as_array().unwrap().last().unwrap()["ok"], false);
    assert_eq!(info["paid_out"], part);
    assert!(t.balance(&t.vault("shop")).await? >= vault_before);

    // Owner powers stay with the owner.
    failed_with(&t.list(&t.agent, "shop", &s(1)).await?, "only the owner");

    set(1).await?.into_result()?;
    failed_with(&t.pay(&t.agent, "shop", t.carol.id(), "1", none).await?, "operator expired");
    Ok(())
}

/// The whole point: one transaction moves the money, the revenue and the rights.
#[tokio::test]
async fn a_sale_moves_everything_at_once() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(3)).await?.into_result()?;
    t.alice
        .call(t.deed.id(), "set_operator")
        .args_json(json!({"token_id": "shop", "operator_id": t.agent.id(), "budget": s(1), "expires_at_ms": 4_000_000_000_000u64}))
        .deposit(YOCTO)
        .transact()
        .await?
        .into_result()?;
    t.list(&t.alice, "shop", &s(5)).await?.into_result()?;
    let listed = t.info("shop").await?;
    failed_with(&t.buy(&t.alice, "shop", nonce(&listed), near(5)).await?, "already own");
    failed_with(&t.buy(&t.bob, "shop", nonce(&listed), near(4)).await?, "attach the full price");

    let (vault_before, seller_before, buyer_before) =
        (t.balance(&t.vault("shop")).await?, t.balance(t.alice.id()).await?, t.balance(t.bob.id()).await?);
    t.buy(&t.bob, "shop", nonce(&listed), near(6)).await?.into_result()?;

    let info = t.info("shop").await?;
    assert_eq!(info["owner_id"], t.bob.id().as_str());
    assert_eq!(info["price"], Value::Null);
    assert_eq!(info["operator"], Value::Null, "the seller's agent must lose access");
    assert_eq!(info["epoch"], 1);
    assert_eq!(t.balance(t.alice.id()).await? - seller_before, near(5).as_yoctonear());
    let spent = buyer_before - t.balance(t.bob.id()).await?;
    assert!(spent >= near(5).as_yoctonear() && spent < NearToken::from_millinear(5050).as_yoctonear(), "excess refunded, spent {spent}");
    assert_eq!(t.balance(&t.vault("shop")).await?, vault_before, "the money stays with the deed");
    assert_eq!(info["history"].as_array().unwrap().last().unwrap()["kind"], "Sale");

    let none = NearToken::from_yoctonear(0);
    failed_with(&t.pay(&t.agent, "shop", t.carol.id(), "1", none).await?, "not the owner or operator");
    failed_with(&t.pay(&t.alice, "shop", t.alice.id(), "1", YOCTO).await?, "not the owner or operator");
    t.pay(&t.bob, "shop", t.bob.id(), &s(1), YOCTO).await?.into_result()?;

    // Revenue after the sale belongs to the buyer.
    let vault_before = t.balance(&t.vault("shop")).await?;
    t.revenue("shop", near(2)).await?;
    assert_eq!(t.info("shop").await?["earned"], s(2));
    assert_eq!(t.balance(&t.vault("shop")).await? - vault_before, near(2).as_yoctonear());
    Ok(())
}

/// The attack: list a full vault, then drain it while the buyer's transaction is on its way.
#[tokio::test]
async fn draining_after_the_buyer_looked_aborts_the_sale() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    t.revenue("shop", near(5)).await?;
    t.list(&t.alice, "shop", &s(2)).await?.into_result()?;
    let inspected = t.info("shop").await?;

    // Listed means frozen, so the seller has to unlist first.
    failed_with(&t.pay(&t.alice, "shop", t.alice.id(), &s(5), YOCTO).await?, "the deed is listed");

    t.alice.call(t.deed.id(), "unlist").args_json(json!({"token_id": "shop"})).deposit(YOCTO).transact().await?.into_result()?;
    t.pay(&t.alice, "shop", t.alice.id(), &s(5), YOCTO).await?.into_result()?;
    t.list(&t.alice, "shop", &s(2)).await?.into_result()?;

    let before = t.balance(t.bob.id()).await?;
    failed_with(&t.buy(&t.bob, "shop", nonce(&inspected), near(2)).await?, "changed since you inspected");
    assert_eq!(t.info("shop").await?["owner_id"], t.alice.id().as_str());
    assert!(before - t.balance(t.bob.id()).await? < NearToken::from_millinear(50).as_yoctonear(), "the deposit must come back");

    // Revenue does not bump the nonce, so nobody can block a sale by paying the vault.
    let current = t.info("shop").await?;
    t.revenue("shop", near(1)).await?;
    t.buy(&t.bob, "shop", nonce(&current), near(2)).await?.into_result()?;
    assert_eq!(t.info("shop").await?["owner_id"], t.bob.id().as_str());
    Ok(())
}

/// Asynchronous execution: an outflow still running must block any change of hands.
#[tokio::test]
async fn a_sale_cannot_happen_during_a_running_outflow() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    use near_sdk::base64::Engine;
    let args = near_sdk::base64::engine::general_purpose::STANDARD.encode(r#"{"hops":40}"#);
    let running = t
        .alice
        .call(t.deed.id(), "call")
        .args_json(json!({"token_id": "shop", "receiver_id": t.slow.id(), "method": "spin", "args": args, "deposit": "0", "gas_tgas": 220}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(300))
        .transact_async()
        .await?;
    loop {
        if t.info("shop").await?["pending"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    t.list(&t.alice, "shop", &s(1)).await?.into_result()?;
    let listed = t.info("shop").await?;
    assert_eq!(listed["pending"], 1, "the test window closed too early");
    failed_with(&t.buy(&t.bob, "shop", nonce(&listed), near(1)).await?, "still running");
    let gift = t.alice.call(t.deed.id(), "nft_transfer").args_json(json!({"receiver_id": t.bob.id(), "token_id": "shop"})).deposit(YOCTO).gas(Gas::from_tgas(100)).transact().await?;
    failed_with(&gift, "still running");

    running.await?.into_result()?;
    let settled = t.info("shop").await?;
    assert_eq!(settled["pending"], 0);
    t.buy(&t.bob, "shop", nonce(&settled), near(1)).await?.into_result()?;
    assert_eq!(t.info("shop").await?["owner_id"], t.bob.id().as_str());
    Ok(())
}

/// Giving the deed away ends the old holder's rights and listing too.
#[tokio::test]
async fn a_transfer_clears_listing_and_operator() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    t.mint("two", near(1)).await?.into_result()?;
    t.list(&t.alice, "shop", &s(1)).await?.into_result()?;

    let into_vault = t.alice.call(t.deed.id(), "nft_transfer").args_json(json!({"receiver_id": t.vault("two"), "token_id": "shop"})).deposit(YOCTO).gas(Gas::from_tgas(100)).transact().await?;
    failed_with(&into_vault, "a vault cannot own a deed");

    t.alice.call(t.deed.id(), "nft_transfer").args_json(json!({"receiver_id": t.bob.id(), "token_id": "shop"})).deposit(YOCTO).gas(Gas::from_tgas(100)).transact().await?.into_result()?;
    let info = t.info("shop").await?;
    assert_eq!(info["price"], Value::Null, "an old listing must not bind the new owner");
    failed_with(&t.buy(&t.carol, "shop", nonce(&info), near(1)).await?, "not for sale");

    // A transfer that bounces returns the deed but not the rights given up.
    t.alice
        .call(t.deed.id(), "set_operator")
        .args_json(json!({"token_id": "two", "operator_id": t.agent.id(), "budget": s(1), "expires_at_ms": 4_000_000_000_000u64}))
        .deposit(YOCTO)
        .transact()
        .await?
        .into_result()?;
    t.alice
        .call(t.deed.id(), "nft_transfer_call")
        .args_json(json!({"receiver_id": t.slow.id(), "token_id": "two", "msg": "return"}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(150))
        .transact()
        .await?
        .into_result()?;
    let bounced = t.info("two").await?;
    assert_eq!(bounced["owner_id"], t.alice.id().as_str());
    assert_eq!(bounced["operator"], Value::Null);
    assert_eq!(bounced["epoch"], 2);
    Ok(())
}

/// Arbitrary calls are possible but never hidden: the deed stops being sealed and each call stays on record.
#[tokio::test]
async fn arbitrary_calls_unseal_the_deed_for_good() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    assert_eq!(t.info("shop").await?["sealed"], true);

    t.call_from_vault("shop", "spin", r#"{"hops":0}"#).await?.into_result()?;
    let info = t.info("shop").await?;
    assert_eq!(info["sealed"], false);
    assert_eq!(info["calls"][0]["method"], "spin");
    assert_eq!(info["calls"][0]["ok"], true);
    assert_eq!(info["calls"][0]["args_sha256"].as_str().unwrap().len(), 64);

    // A failing call is recorded as failed and does not wedge the deed.
    let _ = t.call_from_vault("shop", "fail", "{}").await?;
    let info = t.info("shop").await?;
    assert_eq!(info["calls"][1]["ok"], false);
    assert_eq!(info["pending"], 0);

    // The vault cannot be turned against the deed contract.
    let at_parent = t
        .alice
        .call(t.deed.id(), "call")
        .args_json(json!({"token_id": "shop", "receiver_id": t.deed.id(), "method": "list", "args": "", "deposit": "0", "gas_tgas": 30}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    failed_with(&at_parent, "cannot call the deed contract");

    // Non-owners cannot use it, and the budgeted operator path does not reach it.
    let stranger = t
        .bob
        .call(t.deed.id(), "call")
        .args_json(json!({"token_id": "shop", "receiver_id": t.slow.id(), "method": "spin", "args": "", "deposit": "0", "gas_tgas": 30}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    failed_with(&stranger, "only the owner");

    // A call that tries to stay open while the seller sells is covered by the running-outflow test;
    // the cap keeps the permanent record bounded.
    for _ in 0..14 {
        t.call_from_vault("shop", "spin", r#"{"hops":0}"#).await?.into_result()?;
    }
    failed_with(&t.call_from_vault("shop", "spin", r#"{"hops":0}"#).await?, "used all its arbitrary calls");
    Ok(())
}

/// A hostile contract returning as much data as it can afford must not starve the callback, or `pending`
/// would stay above zero and the deed could never be sold.
#[tokio::test]
async fn huge_return_value_does_not_wedge_the_deed() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    // 4 MiB is the protocol limit, but at about 69 Mgas per byte 3 MB is what a transaction can afford.
    let result = t.call_from_vault("shop", "big", r#"{"bytes":3000000}"#).await?;
    assert!(result.failures().is_empty(), "this test must really return the data: {:?}", result.failures());
    let info = t.info("shop").await?;
    assert_eq!(info["pending"], 0, "the callback must have run");
    assert_eq!(info["calls"][0]["ok"], true);
    t.list(&t.alice, "shop", &s(1)).await?.into_result()?;
    t.buy(&t.bob, "shop", nonce(&t.info("shop").await?), near(1)).await?.into_result()?;
    Ok(())
}

/// Tokens and NFTs the vault holds go with the deed, and only the current owner can send them.
#[tokio::test]
async fn inventory_moves_with_the_deed() -> anyhow::Result<()> {
    let t = setup().await?;
    t.mint("shop", near(1)).await?.into_result()?;
    let vault = t.vault("shop");
    t.slow.call("ft_mint").args_json(json!({"account_id": vault, "amount": "1000"})).transact().await?.into_result()?;
    t.slow.call("nft_give").args_json(json!({"account_id": vault})).transact().await?.into_result()?;

    let ft_pay = |who: Account, amount: &'static str| {
        let (deed, slow, carol) = (t.deed.id().clone(), t.slow.id().clone(), t.carol.id().clone());
        async move {
            anyhow::Ok(
                who.call(&deed, "ft_pay")
                    .args_json(json!({"token_id": "shop", "token": slow, "receiver_id": carol, "amount": amount, "memo": "supplier"}))
                    .deposit(YOCTO)
                    .gas(Gas::from_tgas(150))
                    .transact()
                    .await?,
            )
        }
    };
    ft_pay(t.alice.clone(), "100").await?.into_result()?;
    let carol_tokens: String = t.slow.view("ft_balance_of").args_json(json!({"account_id": t.carol.id()})).await?.json()?;
    assert_eq!(carol_tokens, "100");
    assert_eq!(t.info("shop").await?["sealed"], true, "a typed payment keeps the deed sealed");

    t.list(&t.alice, "shop", &s(1)).await?.into_result()?;
    t.buy(&t.bob, "shop", nonce(&t.info("shop").await?), near(1)).await?.into_result()?;
    failed_with(&ft_pay(t.alice.clone(), "100").await?, "only the owner");
    ft_pay(t.bob.clone(), "100").await?.into_result()?;

    t.bob
        .call(t.deed.id(), "nft_pay")
        .args_json(json!({"token_id": "shop", "nft_contract": t.slow.id(), "nft_token_id": "sword", "receiver_id": t.bob.id()}))
        .deposit(YOCTO)
        .gas(Gas::from_tgas(150))
        .transact()
        .await?
        .into_result()?;
    let holder: Option<String> = t.slow.view("nft_holder").await?.json()?;
    assert_eq!(holder.as_deref(), Some(t.bob.id().as_str()));
    Ok(())
}
