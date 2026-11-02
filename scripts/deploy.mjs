// Deploys the deed contract to testnet with no access key: nobody, including the deployer, can change
// it afterwards. Usage: node scripts/deploy.mjs <base-account>.testnet <contract-name>
import fs from "node:fs";
import { actions, exists, faucetAccount, keysOf, near, balance, nearToYocto, signer, teraToGas, txLink, view } from "./lib.mjs";

const [base, name] = process.argv.slice(2);
if (!base || !name) throw new Error("usage: deploy.mjs <base>.testnet <contract-name>");
const contract = `${name}.${base}`;

await faucetAccount(base);
console.log(`base ${base}: ${near(await balance(base))} NEAR`);
if (await exists(contract)) throw new Error(`${contract} already exists`);

const code = fs.readFileSync("target/near/deed_contract/deed_contract.wasm");
const vault = fs.readFileSync("target/near/deed_vault/deed_vault.wasm");
// Borsh for a single Vec<u8> argument: a little-endian u32 length, then the bytes.
const init = Buffer.alloc(4 + vault.length);
init.writeUInt32LE(vault.length, 0);
vault.copy(init, 4);

const res = await signer(base).signAndSendTransaction({
  receiverId: contract,
  actions: [
    actions.createAccount(),
    actions.transfer(nearToYocto("3.5")),
    actions.deployContract(code),
    actions.functionCall("new", init, teraToGas("100"), 0n),
  ],
});
console.log("deployed", contract, txLink(res.transaction.hash));
console.log("access keys on the contract:", (await keysOf(contract)).length);
console.log("vault code hash:", await view(contract, "vault_code_hash"));
