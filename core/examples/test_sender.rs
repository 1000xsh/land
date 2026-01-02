//! test sender for land-core RPC server
//!
//! usage:
//!   cargo run -p land-core --example test_sender -- --keypair ./auth.json --fanout 3

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

const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";
const DEFAULT_SOLANA_RPC: &str = "http://45.152.160.253:8899";
const DEFAULT_PRIORITY_FEE: u64 = 1000; // micro-lamports per compute unit

#[derive(serde::Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: SendTransactionParams,
}

#[derive(serde::Serialize)]
struct SendTransactionParams {
    transaction: String,
    fanout: usize,
    target_slot: u64,
}

#[derive(serde::Deserialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: serde_json::Value,
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let server_url = arg_value(&args, "--server")
        .or_else(|| env::var("SERVER_URL").ok())
        .unwrap_or_else(|| DEFAULT_SERVER.into());

    let solana_rpc_url = arg_value(&args, "--rpc")
        .or_else(|| env::var("SOLANA_RPC").ok())
        .unwrap_or_else(|| DEFAULT_SOLANA_RPC.into());

    let keypair_path = arg_value(&args, "--keypair").or_else(|| env::var("KEYPAIR_PATH").ok());

    let fanout: usize = arg_value(&args, "--fanout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let priority_fee: u64 = arg_value(&args, "--priority-fee")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PRIORITY_FEE);

    let compute_unit_limit: Option<u32> =
        arg_value(&args, "--compute-limit").and_then(|s| s.parse().ok());

    println!("land-core test sender");
    println!("server: {}", server_url);
    println!("solana rpc: {}", solana_rpc_url);
    println!("fanout: {}", fanout);
    println!("priority fee: {} micro-lamports/CU", priority_fee);
    if let Some(limit) = compute_unit_limit {
        println!("compute unit limit: {}", limit);
    }
    println!();

    // load or create keypair
    let payer = load_keypair(keypair_path.as_deref())?;
    println!("payer: {}", payer.pubkey());

    let solana_rpc =
        RpcClient::new_with_commitment(solana_rpc_url.clone(), CommitmentConfig::processed());

    match solana_rpc.get_balance(&payer.pubkey()).await {
        Ok(balance) => println!(
            "balance: {} lamports ({:.6} SOL)",
            balance,
            balance as f64 / 1e9
        ),
        Err(e) => println!("could not fetch balance: {}", e),
    }

    // recipient
    let recipient = Pubkey::from_str("")?;
    let amount: u64 = 1000; // 0.000001 SOL

    println!("recipient: {}", recipient);
    println!("amount: {} lamports", amount);
    println!();

    println!("fetching blockhash...");
    let recent_blockhash = solana_rpc.get_latest_blockhash().await?;
    println!("blockhash: {}", recent_blockhash);

    let mut instructions = Vec::new();

    if let Some(limit) = compute_unit_limit {
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(limit));
        println!("setting compute unit limit: {}", limit);
    }

    instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
        priority_fee,
    ));
    println!(
        "setting compute unit price: {} micro-lamports",
        priority_fee
    );

    // create transfer instruction (system program)
    let system_program_id = Pubkey::from_str("11111111111111111111111111111111")?;
    let mut instruction_data = vec![2, 0, 0, 0]; // transfer variant = 2
    instruction_data.extend_from_slice(&amount.to_le_bytes());

    let transfer_ix = Instruction::new_with_bytes(
        system_program_id,
        &instruction_data,
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(recipient, false),
        ],
    );

    instructions.push(transfer_ix);

    let current_slot = solana_rpc.get_slot().await.unwrap_or(0);

    // create memo instruction
    let memo_program_id = Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr")?;
    let memo_text = format!("land test {}", current_slot);
    let memo_ix = Instruction::new_with_bytes(
        memo_program_id,
        memo_text.as_bytes(),
        vec![AccountMeta::new_readonly(payer.pubkey(), true)],
    );
    instructions.push(memo_ix);

    println!("memo: {}", memo_text);

    // create and sign transaction
    let message = Message::new(&instructions, Some(&payer.pubkey()));
    let mut transaction = Transaction::new_unsigned(message);
    transaction.sign(&[&payer], recent_blockhash);

    println!();
    println!("transaction created");
    println!("signature: {}", transaction.signatures[0]);

    let serialized = bincode::serialize(&transaction)?;
    println!("size: {} bytes", serialized.len());

    if serialized.len() > 1232 {
        return Err("transaction too large".into());
    }

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&serialized);

    // get current slot for target_slot
    // send to land-core
    println!();
    println!("sending to land-core...");

    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "sendTransaction",
        params: SendTransactionParams {
            transaction: encoded,
            fanout,
            target_slot: current_slot,
        },
    };

    let start = Instant::now();

    let client = reqwest::Client::new();
    let response = client.post(&server_url).json(&request).send().await?;

    let elapsed = start.elapsed();
    let status = response.status();

    println!("status: {}", status);
    println!("time: {:?}", elapsed);

    let body: JsonRpcResponse = response.json().await?;

    if let Some(result) = body.result {
        println!("result: {}", serde_json::to_string_pretty(&result)?);
    }

    if let Some(error) = body.error {
        println!("error: {}", serde_json::to_string_pretty(&error)?);
    }

    println!();
    println!("signature: {}", transaction.signatures[0]);
    println!(
        "explorer: https://explorer.solana.com/tx/{}?cluster=custom&customUrl={}",
        transaction.signatures[0],
        urlencoding::encode(&solana_rpc_url)
    );

    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn load_keypair(path: Option<&str>) -> Result<Keypair, Box<dyn std::error::Error>> {
    if let Some(path) = path {
        if std::path::Path::new(path).exists() {
            println!("loading keypair from: {}", path);
            let data = std::fs::read_to_string(path)?;
            let bytes: Vec<u8> = serde_json::from_str(&data)?;

            if bytes.len() < 32 {
                return Err("invalid keypair: too short".into());
            }

            let secret_key: [u8; 32] = bytes[0..32].try_into()?;
            return Ok(Keypair::new_from_array(secret_key));
        } else {
            return Err(format!("keypair file not found: {}", path).into());
        }
    }

    println!("creating ephemeral keypair (no --keypair provided)");
    Ok(Keypair::new())
}
