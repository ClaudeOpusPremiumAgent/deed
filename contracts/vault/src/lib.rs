//! The account a deed owns. It holds money, tokens and NFTs and can only do what the deed contract
//! tells it. It has no access keys, no state and no dependencies, and it cannot add keys, deploy code
//! or delete itself: those host functions are not even imported below, which you can verify in the
//! compiled wasm. The authority is derived from the account name, so there is no init call to get wrong.
//!
//! Instructions from the deed contract are borsh-encoded. Nothing else can reach them.
#![cfg(target_arch = "wasm32")]
#![no_std]

use core::ptr::addr_of_mut;
use core::slice;

#[link(wasm_import_module = "env")]
extern "C" {
    fn input(register_id: u64);
    fn register_len(register_id: u64) -> u64;
    fn read_register(register_id: u64, ptr: u64);
    fn current_account_id(register_id: u64);
    fn predecessor_account_id(register_id: u64);
    fn promise_batch_create(account_id_len: u64, account_id_ptr: u64) -> u64;
    fn promise_batch_action_transfer(promise_index: u64, amount_ptr: u64);
    fn promise_batch_action_function_call(
        promise_index: u64,
        method_name_len: u64,
        method_name_ptr: u64,
        arguments_len: u64,
        arguments_ptr: u64,
        amount_ptr: u64,
        gas: u64,
    );
    fn promise_return(promise_id: u64);
    fn value_return(value_len: u64, value_ptr: u64);
    fn panic_utf8(len: u64, ptr: u64) -> !;
}

const TGAS: u64 = 1_000_000_000_000;
const BUF_SIZE: usize = 512 * 1024;
static mut BUF: [u8; BUF_SIZE] = [0; BUF_SIZE];
static mut USED: usize = 0;

#[panic_handler]
fn on_panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

fn fail(message: &str) -> ! {
    unsafe { panic_utf8(message.len() as u64, message.as_ptr() as u64) }
}

/// Fills register 0 with `fill` and copies it into the scratch buffer. A call is short-lived, so
/// nothing is ever freed.
fn read(fill: unsafe extern "C" fn(u64)) -> &'static [u8] {
    unsafe {
        fill(0);
        let len = register_len(0) as usize;
        let used = USED;
        if len > BUF_SIZE - used {
            fail("arguments too large");
        }
        let start = (addr_of_mut!(BUF) as *mut u8).add(used);
        read_register(0, start as u64);
        USED = used + len;
        slice::from_raw_parts(start, len)
    }
}

fn only_deed_contract() {
    let me = read(current_account_id);
    let caller = read(predecessor_account_id);
    match me.iter().position(|&b| b == b'.') {
        Some(dot) if caller == &me[dot + 1..] => {}
        _ => fail("only the deed contract can do this"),
    }
}

struct Args(&'static [u8]);

impl Args {
    fn take(&mut self, n: usize) -> &'static [u8] {
        if n > self.0.len() {
            fail("bad arguments");
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        head
    }

    /// Borsh string or byte vector: a little-endian u32 length, then the bytes.
    fn bytes(&mut self) -> &'static [u8] {
        let len = self.take(4);
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        self.take(len)
    }

    fn u64(&mut self) -> u64 {
        let b = self.take(8);
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }
}

/// (receiver_id: String, amount: u128)
#[no_mangle]
pub extern "C" fn send() {
    only_deed_contract();
    let mut args = Args(read(input));
    let receiver = args.bytes();
    let amount = args.take(16);
    unsafe {
        let promise = promise_batch_create(receiver.len() as u64, receiver.as_ptr() as u64);
        promise_batch_action_transfer(promise, amount.as_ptr() as u64);
        promise_return(promise);
    }
}

/// (receiver_id: String, method: String, args: Vec<u8>, deposit: u128, gas_tgas: u64)
#[no_mangle]
pub extern "C" fn call() {
    only_deed_contract();
    let mut args = Args(read(input));
    let receiver = args.bytes();
    let method = args.bytes();
    let arguments = args.bytes();
    let deposit = args.take(16);
    let gas = args.u64();
    if receiver == read(current_account_id) {
        fail("a vault cannot call itself");
    }
    unsafe {
        let promise = promise_batch_create(receiver.len() as u64, receiver.as_ptr() as u64);
        promise_batch_action_function_call(
            promise,
            method.len() as u64,
            method.as_ptr() as u64,
            arguments.len() as u64,
            arguments.as_ptr() as u64,
            deposit.as_ptr() as u64,
            gas.saturating_mul(TGAS),
        );
        promise_return(promise);
    }
}

/// NEP-141 receiver: keep everything.
#[no_mangle]
pub extern "C" fn ft_on_transfer() {
    let keep_all = b"\"0\"";
    unsafe { value_return(keep_all.len() as u64, keep_all.as_ptr() as u64) }
}

/// NEP-171 receiver: keep the token.
#[no_mangle]
pub extern "C" fn nft_on_transfer() {
    let keep = b"false";
    unsafe { value_return(keep.len() as u64, keep.as_ptr() as u64) }
}
