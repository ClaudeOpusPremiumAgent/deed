# Threat model

The question: every way a seller could try to keep the money or the control after selling a deed, and what stops it. Test names refer to [tests/tests/deed.rs](tests/tests/deed.rs).

## The seller tries to keep the money

| Attempt | What stops it | Test |
| --- | --- | --- |
| Withdraw while the deed is listed | Listed means frozen: `pay`, `ft_pay`, `nft_pay` and `call` all refuse | `draining_after_the_buyer_looked_aborts_the_sale` |
| Unlist, drain, relist at the same price after the buyer looked | Each step bumps the nonce, and `buy` requires the nonce the buyer named, so the deposit returns | same |
| Change the price after the buyer looked | `list` bumps the nonce, which pins the price too | same |
| Start a slow outflow, then sell while it runs | `pending` stays above zero until the callback, and `buy` and transfers refuse | `a_sale_cannot_happen_during_a_running_outflow` |
| Make the callback run out of gas so that state goes wrong | Callback gas is reserved when the outflow starts and covers the largest return value a callee can afford (about 3 MB). If the reservation does not fit in the transaction, the whole transaction reverts | `huge_return_value_does_not_wedge_the_deed` |
| Have a failed payment count as paid | The callback records the real result, and a failed payment returns the budget and the money | `operator_is_held_to_budget_and_expiry` |

## The seller tries to keep control

| Attempt | What stops it | Test |
| --- | --- | --- |
| Keep an access key to the vault | The vault never has one, and its code does not import key functions | `mint_creates_a_keyless_vault`, `the_vault_code_cannot_add_keys_deploy_or_delete` |
| Instruct the vault directly | It only obeys its parent, derived from its own account name | `revenue_and_owner_payments` |
| Keep a budgeted agent running | `change_hands` clears the operator, on every path | `a_sale_moves_everything_at_once` |
| Keep an approval on the deed | NEP-178 approvals are not implemented | not applicable |
| Leave a listing that binds the new owner | `change_hands` clears the price | `a_transfer_clears_listing_and_operator` |
| Use the vault against the deed contract | Calls to the deed contract and to the vault itself are refused | `arbitrary_calls_unseal_the_deed_for_good` |
| Give the deed to its own or another vault, locking it forever | Refused on every path into a vault | `a_transfer_clears_listing_and_operator` |
| Bounce a transfer through `nft_transfer_call` to get rights back | The deed returns, but the operator stays cleared and the epoch moves on | `a_transfer_clears_listing_and_operator` |
| Upgrade the contract or swap the vault code | The deed contract was created with no key and has no upgrade method. Vault code is an immutable global contract addressed by hash | checked live in the data room |
| Initialise the contract first with hostile vault code | `new` runs in the transaction that creates the account, and only that account or its parent may call it | `mint_creates_a_keyless_vault` |

## What is not stopped, and what is done instead

| Attempt | Why it cannot be stopped | What the buyer gets instead |
| --- | --- | --- |
| Leave rights on another contract, such as `nft_approve` on an NFT the vault holds, or a position credited to the seller | Only the owner's arbitrary `call` can do this, and the contract cannot understand other contracts | Each such call is kept for the life of the deed with its receiver, method and args hash, and the deed stops being `sealed`. A sealed deed has made only plain and token payments, so it cannot have done this |
| Pay the deed from your own accounts to fake revenue | Money is money | The data room lists who paid. `earned` counts only `pay_in` |
| Push old entries out of the history with many small actions | History is bounded so that storage costs stay bounded | Arbitrary calls are never pushed out, and the chain keeps everything. The nonce shows how many actions there were |
| Sell something off-chain along with the deed, and keep it | Domains, customers and reputation are not on-chain | Stated in the README: only on-chain value moves |
| Delete the seller account before the proceeds arrive | The payment then bounces to the contract and stays there | This only hurts the seller |

## Griefing

| Attempt | What stops it | Test |
| --- | --- | --- |
| Block a sale by paying the deed | Revenue does not bump the nonce | `draining_after_the_buyer_looked_aborts_the_sale` |
| Block a sale as a third party | Only the owner can bump the nonce. A seller doing it only delays their own sale | same |
| Push real customers off the revenue list with dust | Payments under 0.001 NEAR count as revenue but are not listed | not tested |
| Exhaust the contract's storage money with long notes | Notes are cut to 64 bytes, not characters. All per-deed lists are bounded, and the mint deposit pays for their maximum size up front | `revenue_and_owner_payments` |
| Mint with a name that is taken | Nothing is minted and the deposit returns | `mint_creates_a_keyless_vault` |

## Why `buy` has no reentrancy window

`buy` makes every state change in one receipt: it checks, clears the old rights and moves the NFT. Only then does it schedule the payment to the seller and the refund to the buyer, and these are plain transfers that run no code on the receiving side. No callback can observe a half-finished sale.

## What you must trust

- The NEAR protocol, including its guarantee that an account with no keys cannot sign.
- For the data room, the RPC node it reads from, `test.rpc.fastnear.com`. The commands in the README let you check any figure against another node.
- This code, which has not been audited.
