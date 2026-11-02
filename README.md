# Deed

An NFT that owns a NEAR account. Sell the NFT and the money, the revenue and the right to run that account move to the buyer in one transaction, and the seller cannot empty the account between the buyer's check and the payment.

Built at NEAR HACKATHON. Track `nfts`, also fits `business`. Testnet only and not audited.

- **Live data room:** https://claudeopuspremiumagent.github.io/deed/?contract=v1.deed-hq.testnet&deed=desk-5
- **Deployed contract:** `v1.deed-hq.testnet`
- **Threat model:** [THREAT_MODEL.md](THREAT_MODEL.md)

## The problem

Small online businesses and AI agents are sold by handing over logins: wallet keys, API accounts, passwords. The seller can keep a copy, and the buyer is paying for a balance that can leave at any moment.

Putting the business in an NFT-owned account fixes ownership but opens a new scam, which ERC-6551 (token-bound accounts on Ethereum) [describes directly](https://eips.ethereum.org/EIPS/eip-6551): the seller withdraws the assets just before the sale closes. That standard leaves protection to marketplaces. NEAR makes it harder, because execution is asynchronous: a withdrawal can already be running when the sale is processed.

## What it does

1. `mint` creates a real sub-account, `<token>.<contract>`, that runs a 2 KB immutable contract and has **no access keys**. Nobody can sign for it, including the seller and the deployer.
2. Customers `pay_in`. The money goes to that account and the revenue is recorded.
3. The owner spends with `pay`, `ft_pay`, `nft_pay` or `call`. An agent can run the business as an operator: it signs with a function-call key, within a budget and an expiry.
4. The owner can `list` the deed for sale. While it is listed nothing can leave the account.
5. The buyer checks the state and calls `buy(token_id, expected_nonce)`. If anything changed since they looked, the purchase aborts and the deposit returns. If not, ownership moves and the old owner's and operator's rights end, in that one receipt.

## Why it holds under asynchronous execution

Every outflow goes through the deed contract, which does two things before it sends anything:

- `nonce += 1`. So the nonce changes whenever money could have left, and whenever the price or the operator changes.
- `pending += 1`, undone by a callback when the outflow finishes. So the contract knows whether anything is still running.

`buy` runs in a single receipt on the deed contract and requires `nonce == expected_nonce` and `pending == 0`. Nothing asynchronous happens between the check and the change of ownership.

- A seller who drains and relists at the same price changes the nonce, so the buyer's transaction aborts.
- A stale read by the buyer can only make a purchase fail, never succeed at the wrong state.
- Revenue does not bump the nonce, so paying the vault cannot block a sale, and it can only help the buyer.

## Try it

Everything runs in Docker and mounts only this directory. You need Docker, and about 5 minutes for the first build.

### See it live

Open the data room link above. You should see five `PASS` checks, 0.750 NEAR of recorded revenue from `customer.deed-hq.testnet`, and a history that includes the seller's withdrawal and the final sale. The page calls NEAR RPC from your browser, with no backend. To check one claim yourself:

```sh
curl -s https://test.rpc.fastnear.com -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_access_key_list","finality":"final","account_id":"desk-5.v1.deed-hq.testnet"}}'
```

You should see `"keys":[]`. The same request for `v1.deed-hq.testnet` also returns no keys.

### Run the tests

```sh
docker build -t deed-dev -f Dockerfile.dev .
./dev.sh test
```

You should see `test result: ok. 11 passed`. The tests run on a local NEAR sandbox, and each one states the property it protects, in [tests/tests/deed.rs](tests/tests/deed.rs).

The build also reproduces the vault code that is on chain. This should print `2cdd58a9e71f7a64bb3e14bf250d14ceeb0fcc35bf75ff66abc9b02d2e4d5c65`, the same value as `vault_code_hash` on the contract:

```sh
sha256sum target/near/deed_vault/deed_vault.wasm
```

### Run the whole story, attack included, on testnet

This creates your own accounts with the public faucet, deploys your own copy of the contract and prints a transaction link for every step. Testnet keys are written to `.secrets/`, which git ignores.

```sh
docker build -t deed-dev -f Dockerfile.dev .
./dev.sh build
./dev.sh npm ci --ignore-scripts
./dev.sh node scripts/deploy.mjs YOUR-NEW-NAME.testnet deed
./dev.sh node scripts/demo.mjs YOUR-NEW-NAME.testnet deed.YOUR-NEW-NAME.testnet
```

You should see `BLOCKED buyer sends 0.3 NEAR, naming the nonce they inspected` with the contract's reason, `the deed changed since you inspected it`, and then a fair sale that succeeds. The script exits with an error if any step that should be refused goes through.

## Contract API

Deed contract (NEP-171 core, enumeration and metadata):

| Method | Who | What |
| --- | --- | --- |
| `mint(token_id, title, description?, receiver_id?)` | anyone, at least 0.36 NEAR attached | Creates the keyless vault, then mints. 0.35 NEAR pays for state, the rest seeds the vault. |
| `pay_in(token_id, memo?)` | anyone | Revenue. Goes to the vault and does not bump the nonce. |
| `pay(token_id, receiver_id, amount, memo?)` | owner (1 yocto) or operator | Pays NEAR from the vault. |
| `ft_pay(token_id, token, receiver_id, amount, memo?)` | owner | Pays a NEP-141 token from the vault. |
| `nft_pay(token_id, nft_contract, nft_token_id, receiver_id)` | owner | Sends an NFT the vault holds. |
| `call(token_id, receiver_id, method, args, deposit, gas_tgas)` | owner | Any call from the vault. Kept on record for good, and the deed stops being `sealed`. Limited to 16 per deed. |
| `set_operator(token_id, operator_id, budget, expires_at_ms)`, `revoke_operator(token_id)` | owner | The account allowed to use `pay`, within a budget. |
| `list(token_id, price)`, `unlist(token_id)` | owner | Puts the deed up for sale or takes it off. Listed means frozen. |
| `buy(token_id, expected_nonce)` | anyone, price attached | Buys at the state the buyer inspected. Excess is refunded. |
| `nft_transfer`, `nft_transfer_call` | owner | Gives the deed away. Ends the listing and the operator too. |
| `deed_info(token_id)`, `vault_code_hash()` | view | Everything the data room shows. |

Vault contract ([contracts/vault/src/lib.rs](contracts/vault/src/lib.rs), about 150 lines, no dependencies, no state): `send` and `call` work only for the parent deed contract. `ft_on_transfer` and `nft_on_transfer` accept anything. It does not import the host functions for adding keys, deploying code or deleting accounts, and a test checks that in the compiled wasm.

## How NEAR is used

- **Sub-accounts as assets:** the NFT owns an account that can hold anything and be paid by anyone, without that payer knowing about the deed.
- **Keyless accounts:** the protocol guarantees nobody can sign for an account with no access keys. The deed contract was also deployed with no key, in the transaction that created it, so nobody can change it.
- **Function-call keys:** the operator's key can only call `pay` on this contract. The demo shows the protocol refusing its attempt to call `list`, and the contract refusing an overspend.
- **Global contracts:** all vaults share one immutable contract, addressed by hash. A vault stores 132 bytes, so it costs about 0.0013 NEAR of storage. The data room checks the hash on the account against the deed contract.

No NEAR Intents, Chain Signatures or NEAR AI Cloud: this project does not need them.

## Limits

- **Testnet only and not audited.** Do not hold real value in this.
- **Revenue can be faked:** the seller can pay their own deed. The data room lists who paid, and a buyer should read it.
- **A deed that made arbitrary calls is not sealed:** those calls could have left an approval or a deposit on another contract. The contract cannot judge them, so it keeps them on record and says so.
- **Only on-chain value moves:** customers, domains and reputation do not transfer with the deed.
- **No NEP-178 approvals, on purpose:** an outside marketplace could not apply these checks, so the only way to sell is `list` and `buy`.
- **No wallet button yet:** the data room is read-only and gives the exact CLI command to buy.
- **History keeps the latest 64 entries** and revenue the latest 20. The chain keeps everything, and arbitrary calls are never dropped.
- **If a seller deletes their account before a sale pays out**, the payment bounces back to the contract and stays there.

## Layout

```
contracts/deed     the NFT, the rules, and the data the data room reads
contracts/vault    the keyless account, no dependencies
contracts/slow     test helper: slow, failing and huge-return calls, mock tokens
tests/tests        sandbox tests
scripts            deploy to testnet and run the demo
docs               the data room, one static page (GitHub Pages)
```

## How this was built

All code, tests and documents here were written by Claude Opus 5.5, an AI model, working as an agent with a human operator. First commit 2026-11-02, inside the hackathon window, and no earlier code is used.

Libraries: `near-sdk` and `near-contract-standards` 5.30.0, `near-workspaces` 0.24.0 for tests, `near-api-js` 7.4.0 for scripts. MIT license.
