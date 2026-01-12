//! wincode serialization for solana transactions
//!

use solana_address::Address;
use solana_message::legacy::Message;
use solana_sdk::{hash::Hash, signature::Signature, transaction::Transaction};
use wincode::containers::Vec as WVec;
use wincode::error::WriteResult;
use wincode::{containers::Pod, len::ShortU16Len, SchemaRead, SchemaWrite, Serialize};

// schema definitions for solana transaction wire format
// using ShortU16Len for all Vec fields to match bincodes short_vec encoding

#[derive(SchemaWrite, SchemaRead)]
#[wincode(from = "Transaction")]
struct TransactionSchema {
    #[wincode(with = "WVec<Pod<Signature>, ShortU16Len>")]
    signatures: Vec<Signature>, // short_vec encoded
    message: MessageSchema,
}

#[derive(SchemaWrite, SchemaRead)]
#[wincode(from = "Message")]
struct MessageSchema {
    header: MessageHeaderSchema,
    #[wincode(with = "WVec<Pod<Address>, ShortU16Len>")]
    account_keys: Vec<Address>, // short_vec encoded
    recent_blockhash: Pod<Hash>,
    #[wincode(with = "WVec<CompiledInstructionSchema, ShortU16Len>")]
    instructions: Vec<solana_message::compiled_instruction::CompiledInstruction>, // short_vec encoded
}

#[derive(SchemaWrite, SchemaRead)]
#[wincode(from = "solana_message::MessageHeader")]
struct MessageHeaderSchema {
    num_required_signatures: u8,
    num_readonly_signed_accounts: u8,
    num_readonly_unsigned_accounts: u8,
}

#[derive(SchemaWrite, SchemaRead)]
#[wincode(from = "solana_message::compiled_instruction::CompiledInstruction")]
struct CompiledInstructionSchema {
    program_id_index: u8,
    #[wincode(with = "WVec<u8, ShortU16Len>")]
    accounts: Vec<u8>, // short_vec encoded
    #[wincode(with = "WVec<u8, ShortU16Len>")]
    data: Vec<u8>, // short_vec encoded
}

// public api: serialize transaction to bytes
pub fn serialize_transaction(tx: &Transaction) -> WriteResult<Vec<u8>> {
    TransactionSchema::serialize(tx)
}

// optional: get serialized size without allocating
#[allow(dead_code)]
pub fn transaction_size(tx: &Transaction) -> WriteResult<usize> {
    TransactionSchema::size_of(tx)
}
