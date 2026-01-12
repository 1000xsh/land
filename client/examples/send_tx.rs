//! send transaction via land client.
//!
//! usage:
//!   cargo run -p land_client --example send_tx -- --keypair ./auth.json --fanout 3
//!
//! options:
//!   --server       land server address
//!   --rpc          solana rpc url
//!   --keypair      path to keypair json file
//!   --fanout       number of leaders to send to (default: 1)
//!   --recipient    recipient pubkey
//!   --amount       amount in lamports (default: 1000)
//!   --priority-fee priority fee in micro-lamports per CU (default: 1000)

mod wincode_tx;

use land_client::{LandClient, SendOptions};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::env;
use std::str::FromStr;
use std::time::Instant;

const DEFAULT_SERVER: &str = "https://tx.land.fast";
const DEFAULT_RPC: &str = "https://api.mainnet-beta.solana.com";
const DEFAULT_PRIORITY_FEE: u64 = 1000;
const DEFAULT_AMOUNT: u64 = 1000;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    let server = arg_value(&args, "--server").unwrap_or_else(|| DEFAULT_SERVER.into());
    let rpc_url = arg_value(&args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let keypair_path = arg_value(&args, "--keypair");
    let fanout: usize = arg_value(&args, "--fanout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let recipient_str = arg_value(&args, "--recipient");
    let amount: u64 = arg_value(&args, "--amount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_AMOUNT);
    let priority_fee: u64 = arg_value(&args, "--priority-fee")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PRIORITY_FEE);

    println!("land send_tx example");
    println!("server: {}", server);
    println!("rpc: {}", rpc_url);
    println!("fanout: {}", fanout);
    println!("priority fee: {} micro-lamports/CU", priority_fee);
    println!();

    // load keypair
    let payer = match load_keypair(keypair_path.as_deref()) {
        Ok(kp) => kp,
        Err(e) => {
            eprintln!("failed to load keypair: {}", e);
            return;
        }
    };
    println!("payer: {}", payer.pubkey());

    // recipient (self if not provided)
    let recipient = match recipient_str {
        Some(s) => match Pubkey::from_str(&s) {
            Ok(pk) => pk,
            Err(e) => {
                eprintln!("invalid recipient pubkey: {}", e);
                return;
            }
        },
        None => payer.pubkey(),
    };
    println!("recipient: {}", recipient);
    println!("amount: {} lamports", amount);
    println!();

    // connect to solana rpc
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::processed());

    // get balance
    match rpc.get_balance(&payer.pubkey()).await {
        Ok(balance) => println!("balance: {} lamports ({:.6} SOL)", balance, balance as f64 / 1e9),
        Err(e) => println!("could not fetch balance: {}", e),
    }

    // get blockhash and slot
    println!("fetching blockhash...");
    let blockhash = match rpc.get_latest_blockhash().await {
        Ok(bh) => bh,
        Err(e) => {
            eprintln!("failed to get blockhash: {}", e);
            return;
        }
    };
    println!("blockhash: {}", blockhash);

    let current_slot = rpc.get_slot().await.unwrap_or(0);
    println!("slot: {}", current_slot);
    println!();

    // build transaction
    let mut instructions = Vec::new();

    // set priority fee
    instructions.push(ComputeBudgetInstruction::set_compute_unit_price(priority_fee));

    // transfer instruction
    let system_program = Pubkey::from_str("11111111111111111111111111111111").unwrap();
    let mut transfer_data = vec![2, 0, 0, 0]; // transfer variant
    transfer_data.extend_from_slice(&amount.to_le_bytes());

    instructions.push(Instruction::new_with_bytes(
        system_program,
        &transfer_data,
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(recipient, false),
        ],
    ));

    let memo_program = Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr").unwrap();
    let memo = format!("land {}", current_slot);
    instructions.push(Instruction::new_with_bytes(
        memo_program,
        memo.as_bytes(),
        vec![AccountMeta::new_readonly(payer.pubkey(), true)],
    ));

    // create and sign
    let message = Message::new(&instructions, Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[&payer], blockhash);

    println!("transaction created");
    println!("signature: {}", tx.signatures[0]);

    // serialize
    let tx_bytes = match wincode_tx::serialize_transaction(&tx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to serialize: {}", e);
            return;
        }
    };
    println!("size: {} bytes", tx_bytes.len());

    if tx_bytes.len() > 1232 {
        eprintln!("transaction too large");
        return;
    }
    println!();

    // connect to land server
    println!("connecting to land server...");
    let mut land = match LandClient::connect(&server) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {}", e);
            return;
        }
    };
    println!("connected");

    // send transaction
    println!("sending...");
    let opts = SendOptions::default()
        .fanout(fanout)
        .target_slot(current_slot);

    let start = Instant::now();
    let result = land.send_with_opts(&tx_bytes, &opts);
    let elapsed = start.elapsed();

    match result {
        Ok(r) => {
            println!("queued: request_id={}", r.request_id);
            println!("time: {:?}", elapsed);
        }
        Err(e) => {
            eprintln!("send failed: {}", e);
            eprintln!("time: {:?}", elapsed);
        }
    }

    println!();
    println!("signature: {}", tx.signatures[0]);
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn load_keypair(path: Option<&str>) -> Result<Keypair, Box<dyn std::error::Error>> {
    if let Some(p) = path {
        if !std::path::Path::new(p).exists() {
            return Err(format!("keypair file not found: {}", p).into());
        }
        println!("loading keypair from: {}", p);
        let data = std::fs::read_to_string(p)?;
        let bytes: Vec<u8> = serde_json::from_str(&data)?;
        if bytes.len() < 32 {
            return Err("invalid keypair: too short".into());
        }
        let secret: [u8; 32] = bytes[0..32].try_into()?;
        return Ok(Keypair::new_from_array(secret));
    }

    println!("creating ephemeral keypair");
    Ok(Keypair::new())
}
