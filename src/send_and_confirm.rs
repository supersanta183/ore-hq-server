use std::{sync::Arc, time::Duration};

use colored::Colorize;
use solana_sdk::{commitment_config::CommitmentLevel, compute_budget::ComputeBudgetInstruction, instruction::Instruction, native_token::{lamports_to_sol, sol_to_lamports}, signature::{Keypair, Signature}, signer::Signer, transaction::Transaction};
use solana_client::{client_error::{ClientError, ClientErrorKind, Result as ClientResult}, nonblocking::rpc_client::{self, RpcClient}, rpc_config::RpcSendTransactionConfig};
use solana_rpc_client::spinner;
use solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding};

const MIN_SOL_BALANCE: f64 = 0.0005;

const RPC_RETRIES: usize = 0;
const _SIMULATION_RETRIES: usize = 4;
const GATEWAY_RETRIES: usize = 150;
const CONFIRM_RETRIES: usize = 1;

const CONFIRM_DELAY: u64 = 0;
const GATEWAY_DELAY: u64 = 400;

pub enum ComputeBudget {
    Dynamic,
    Fixed(u32),
}

pub async fn send_and_confirm_tx(
    ixs: &[Instruction],
    compute_budget: ComputeBudget,
    skip_confirm: bool,
    priority_fee: u64,
    signer: &Arc<Keypair>,
    rpc_client: Arc<RpcClient>,
) -> ClientResult<Signature> {
    let progress_bar = spinner::new_progress_bar();
    let signer = signer;
    let client = rpc_client.clone();
    let client2 = rpc_client.clone();

    // Return error if balance is zero
    let balance = client.get_balance(&signer.pubkey()).await?;
    if balance <= sol_to_lamports(MIN_SOL_BALANCE) {
        return Err(ClientError {
            request: None,
            kind: ClientErrorKind::Custom(format!(
                "Insufficient balance: {} SOL\nPlease top up with at least {} SOL",
                lamports_to_sol(balance),
                MIN_SOL_BALANCE
            )),
        });
    }

    // Set compute units
    let mut final_ixs = vec![];
    match compute_budget {
        ComputeBudget::Dynamic => {
            // TODO simulate
            //final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000))
        }
        ComputeBudget::Fixed(cus) => {
            //final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cus))
        }
    }
    //final_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(priority_fee));
    final_ixs.extend_from_slice(ixs);

    // Build tx
    let send_cfg = RpcSendTransactionConfig {
        skip_preflight: true,
        preflight_commitment: Some(CommitmentLevel::Confirmed),
        encoding: Some(UiTransactionEncoding::Base64),
        max_retries: Some(RPC_RETRIES),
        min_context_slot: None,
    };
    let mut tx = Transaction::new_with_payer(&final_ixs, Some(&signer.pubkey()));

    // Sign tx
    let (hash, _slot) = client
        .get_latest_blockhash_with_commitment(rpc_client.commitment())
        .await
        .map_err(|err| ClientError {
            request: None,
            kind: ClientErrorKind::Custom(err.to_string()),
        })?;
    tx.sign(&[&signer], hash);

    // Submit tx
    let mut attempts = 0;
    loop {
        progress_bar.set_message(format!("Submitting transaction... (attempt {})", attempts));
        match client2.send_transaction_with_config(&tx, send_cfg).await {
            Ok(sig) => {
                // Skip confirmation
                if skip_confirm {
                    progress_bar.finish_with_message(format!("Sent: {}", sig));
                    return Ok(sig);
                }

                // Confirm the tx landed
                for _ in 0..CONFIRM_RETRIES {
                    tokio::time::sleep(Duration::from_millis(CONFIRM_DELAY)).await;
                    match client.get_signature_statuses(&[sig]).await {
                        Ok(signature_statuses) => {
                            for status in signature_statuses.value {
                                if let Some(status) = status {
                                    if let Some(err) = status.err {
                                        progress_bar.finish_with_message(format!(
                                            "{}: {}",
                                            "ERROR".bold().red(),
                                            err
                                        ));
                                        return Err(ClientError {
                                            request: None,
                                            kind: ClientErrorKind::Custom(err.to_string()),
                                        });
                                    }
                                    if let Some(confirmation) = status.confirmation_status {
                                        match confirmation {
                                            TransactionConfirmationStatus::Processed => {}
                                            TransactionConfirmationStatus::Confirmed
                                            | TransactionConfirmationStatus::Finalized => {
                                                progress_bar.finish_with_message(format!(
                                                    "{} {}",
                                                    "OK".bold().green(),
                                                    sig
                                                ));
                                                return Ok(sig);
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Handle confirmation errors
                        Err(err) => {
                            progress_bar.set_message(format!(
                                "{}: {}",
                                "ERROR".bold().red(),
                                err.kind().to_string()
                            ));
                        }
                    }
                }
            }

            // Handle submit errors
            Err(err) => {
                progress_bar.set_message(format!(
                    "{}: {}",
                    "ERROR".bold().red(),
                    err.kind().to_string()
                ));
            }
        }

        // Retry
        tokio::time::sleep(Duration::from_millis(GATEWAY_DELAY)).await;
        attempts += 1;
        if attempts > GATEWAY_RETRIES {
            progress_bar.finish_with_message(format!("{}: Max retries", "ERROR".bold().red()));
            return Err(ClientError {
                request: None,
                kind: ClientErrorKind::Custom("Max retries".into()),
            });
        }
    }
}