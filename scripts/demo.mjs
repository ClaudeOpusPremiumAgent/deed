// The whole story on testnet, with a transaction link for every step, including the attacks.
// Usage: node scripts/demo.mjs <base>.testnet <contract> [token-id]
import { actions, balance, call, keyFor, keysOf, near, nearToYocto, signer, subAccount, teraToGas, txLink, view, exists } from "./lib.mjs";

const [base, contract, tokenArg] = process.argv.slice(2);
if (!base || !contract) throw new Error("usage: demo.mjs <base>.testnet <contract-account> [token-id]");
const token = tokenArg ?? `shop-${Date.now().toString(36)}`;
const vault = `${token}.${contract}`;
const n = (x) => nearToYocto(String(x));
const s = (x) => n(x).toString();
const YOCTO = 1n;
// RPC nodes can lag a moment behind a transaction that just finished. A buyer acting on a stale read is
// safe, the purchase just aborts, but a demo should show current numbers.
const settled = () => new Promise((r) => setTimeout(r, 3000));
const info = async () => (await settled(), view(contract, "deed_info", { token_id: token }));

const step = (text) => console.log(`\n== ${text}`);
const show = (text) => console.log(`   ${text}`);
const done = (text, hash) => console.log(`   ok   ${text}\n        ${txLink(hash)}`);
async function refused(text, run) {
  try {
    const hash = await run();
    throw new Error(`NOT REFUSED: ${text} (${hash})`);
  } catch (e) {
    if (e.message.startsWith("NOT REFUSED")) throw e;
    const why = (e.message.match(/Smart contract panicked: ([^"\n]*)/) ?? [null, e.message.slice(0, 120)])[1];
    const hash = e.txHash || e.context?.transactionHash || e.transaction?.hash;
    console.log(`   BLOCKED ${text}\n        contract said: ${why}${hash ? `\n        ${txLink(hash)}` : ""}`);
  }
}

// Actors. The agent gets a function-call key only: it can call `pay` on this contract and nothing else.
const seller = await subAccount(base, "seller", 1.5);
const buyer = await subAccount(base, "buyer", 1.5);
const customer = await subAccount(base, "customer", 1.5);
const supplier = await subAccount(base, "supplier", 0.1);
const agent = `agent.${base}`;
if (!(await exists(agent))) {
  await signer(base).signAndSendTransaction({
    receiverId: agent,
    actions: [
      actions.createAccount(),
      actions.transfer(n(0.1)),
      actions.addFunctionCallAccessKey(keyFor(agent).getPublicKey(), contract, ["pay"], n(0.25)),
    ],
  });
}

// Prepaid gas has to be covered up front, so every actor needs some float.
for (const [account, least] of [[seller, 0.9], [buyer, 0.6], [customer, 1], [agent, 0.3]]) {
  const have = await balance(account);
  if (have < n(least)) await signer(base).transfer({ receiverId: account, amount: n(least) - have });
}

step(`1. Mint: "${token}" gets its own account, and nobody holds a key to it`);
done("seller mints the deed", await call(seller, contract, "mint", { token_id: token, title: "Translation desk", description: "Translates documents for pay. Runs itself." }, { deposit: n(0.5) }));
show(`vault account: ${vault}`);
show(`access keys on the vault: ${(await keysOf(vault)).length}`);

step("2. It earns: customers pay the deed");
for (const [amount, memo] of [[0.2, "invoice 1041"], [0.15, "invoice 1042"], [0.25, "invoice 1043"]]) {
  done(`customer pays ${amount} NEAR (${memo})`, await call(customer, contract, "pay_in", { token_id: token, memo }, { deposit: n(amount), gas: 50 }));
}
show(`vault balance: ${near(await balance(vault))} NEAR, recorded revenue: ${near((await info()).earned)} NEAR`);

step("3. It runs itself: an agent spends on a budget, signing with a function-call key");
done("seller sets the agent's budget to 0.1 NEAR", await call(seller, contract, "set_operator", { token_id: token, operator_id: agent, budget: s(0.1), expires_at_ms: Date.now() + 86_400_000 }, { deposit: YOCTO }));
done("agent pays its supplier 0.05 NEAR", await call(agent, contract, "pay", { token_id: token, receiver_id: supplier, amount: s(0.05), memo: "inference" }));
await refused("agent tries to pay itself 0.5 NEAR, far over budget", () => call(agent, contract, "pay", { token_id: token, receiver_id: agent, amount: s(0.5) }));
await refused("agent tries to put the deed up for sale", () => call(agent, contract, "list", { token_id: token, price: s(0.01) }));

step("4. For sale: the buyer inspects the deed");
done("seller lists it for 0.3 NEAR", await call(seller, contract, "list", { token_id: token, price: s(0.3) }, { deposit: YOCTO }));
const inspected = await info();
const inspectedBalance = await balance(vault);
show(`buyer sees: price ${near(inspected.price)} NEAR, vault ${near(inspectedBalance)} NEAR, sealed ${inspected.sealed}, nonce ${inspected.nonce}`);
await refused("seller tries to empty the vault while it is listed", () => call(seller, contract, "pay", { token_id: token, receiver_id: seller, amount: s(0.4) }, { deposit: YOCTO }));

step("5. The attack: the seller drains the vault after the buyer looked, then relists at the same price");
done("seller unlists", await call(seller, contract, "unlist", { token_id: token }, { deposit: YOCTO }));
done("seller takes 0.4 NEAR out of the vault", await call(seller, contract, "pay", { token_id: token, receiver_id: seller, amount: s(0.4) }, { deposit: YOCTO }));
done("seller lists it again for 0.3 NEAR", await call(seller, contract, "list", { token_id: token, price: s(0.3) }, { deposit: YOCTO }));
show(`vault now: ${near(await balance(vault))} NEAR, price still ${near((await info()).price)} NEAR. It looks the same on a marketplace.`);
const before = await balance(buyer);
await refused(`buyer sends 0.3 NEAR, naming the nonce they inspected (${inspected.nonce})`, () => call(buyer, contract, "buy", { token_id: token, expected_nonce: inspected.nonce }, { deposit: n(0.3) }));
await new Promise((r) => setTimeout(r, 3000));
show(`buyer's balance changed by ${near(before - (await balance(buyer)))} NEAR: gas only, the deposit came back`);
show(`owner is still ${(await info()).owner_id}`);

step("6. A fair sale: revenue arriving after the inspection does not get in the way");
done("seller lists it at a price that matches what is left: 0.05 NEAR", await call(seller, contract, "list", { token_id: token, price: s(0.05) }, { deposit: YOCTO }));
const fresh = await info();
show(`buyer inspects again: nonce ${fresh.nonce}, vault ${near(await balance(vault))} NEAR`);
done("a customer pays 0.1 NEAR while the buyer is deciding", await call(customer, contract, "pay_in", { token_id: token, memo: "invoice 1044" }, { deposit: n(0.1), gas: 50 }));
done(`buyer buys, naming nonce ${fresh.nonce}`, await call(buyer, contract, "buy", { token_id: token, expected_nonce: fresh.nonce }, { deposit: n(0.05) }));

step("7. Everything moved in that one transaction");
const after = await info();
show(`owner: ${after.owner_id}`);
show(`operator: ${after.operator ? after.operator.account_id : "none, the seller's agent lost access"}`);
show(`vault: ${near(await balance(vault))} NEAR, access keys ${(await keysOf(vault)).length}`);
await refused("the seller's agent tries to spend", () => call(agent, contract, "pay", { token_id: token, receiver_id: agent, amount: "1" }));
await refused("the seller tries to withdraw", () => call(seller, contract, "pay", { token_id: token, receiver_id: seller, amount: "1" }, { deposit: YOCTO }));
done("the buyer pays from the vault it now controls", await call(buyer, contract, "pay", { token_id: token, receiver_id: buyer, amount: s(0.1), memo: "first withdrawal" }, { deposit: YOCTO }));
done("the next customer's payment now earns for the buyer", await call(customer, contract, "pay_in", { token_id: token, memo: "invoice 1045" }, { deposit: n(0.05), gas: 50 }));

console.log(`\nDone. Deed: ${token}`);
