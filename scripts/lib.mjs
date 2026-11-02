// Shared helpers for the testnet scripts. Keys are testnet-only and stay in .secrets/, which git ignores.
import fs from "node:fs";
import { Account, JsonRpcProvider, KeyPair, actions, nearToYocto, teraToGas } from "near-api-js";

export { actions, nearToYocto, teraToGas };
export const RPC = process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com";
export const provider = new JsonRpcProvider({ url: RPC });
const FAUCET = "https://helper.nearprotocol.com/account";
const SECRETS = ".secrets";
const LATEST = { finality: "optimistic" };

export const near = (yocto) => (Number(BigInt(yocto) / 10n ** 21n) / 1000).toFixed(3);
export const txLink = (hash) => `https://testnet.nearblocks.io/txns/${hash}`;

export function keyFor(accountId) {
  const path = `${SECRETS}/${accountId}.key`;
  fs.mkdirSync(SECRETS, { recursive: true, mode: 0o700 });
  if (!fs.existsSync(path)) fs.writeFileSync(path, KeyPair.fromRandom("ed25519").toString(), { mode: 0o600 });
  return KeyPair.fromString(fs.readFileSync(path, "utf8").trim());
}

export const signer = (accountId) => new Account(accountId, provider, keyFor(accountId).toString());

export async function exists(accountId) {
  try {
    await provider.viewAccount({ accountId, blockQuery: LATEST });
    return true;
  } catch {
    return false;
  }
}

export const balance = async (accountId) => BigInt((await provider.viewAccount({ accountId, blockQuery: LATEST })).amount);

export const keysOf = async (accountId) => (await provider.viewAccessKeyList({ accountId, blockQuery: LATEST })).keys;

export const view = (contractId, method, args = {}) => provider.callFunction({ contractId, method, args, blockQuery: LATEST });

/** Creates a top-level testnet account funded by the public faucet. */
export async function faucetAccount(accountId) {
  if (await exists(accountId)) return;
  const res = await fetch(FAUCET, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ newAccountId: accountId, newAccountPublicKey: keyFor(accountId).getPublicKey().toString() }),
  });
  if (!res.ok) throw new Error(`faucet answered ${res.status}: ${(await res.text()).slice(0, 200)}`);
  for (let i = 0; i < 30 && !(await exists(accountId)); i++) await new Promise((r) => setTimeout(r, 1000));
}

/** Creates a sub-account with its own full-access key, unless it exists already. */
export async function subAccount(parent, name, amount) {
  const accountId = `${name}.${parent}`;
  if (await exists(accountId)) return accountId;
  await signer(parent).signAndSendTransaction({
    receiverId: accountId,
    actions: [actions.createAccount(), actions.transfer(nearToYocto(amount)), actions.addFullAccessKey(keyFor(accountId).getPublicKey())],
  });
  return accountId;
}

/** Sends a call and returns its tx hash. Throws with the contract's own message when it panics. */
export async function call(accountId, contractId, methodName, args, { deposit = 0n, gas = 100 } = {}) {
  const res = await signer(accountId).callFunctionRaw({ contractId, methodName, args, deposit: BigInt(deposit), gas: teraToGas(String(gas)) });
  return res.transaction.hash;
}
