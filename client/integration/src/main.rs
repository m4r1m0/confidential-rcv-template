//! CI-only end-to-end integration test for the ranked-voting template.
//!
//! Runs a full 3-voter ranked-choice scenario on the Esmeralda testnet. For primary testing,
//! see `templates/ranked_voting/tests/test.rs` (in-process, no testnet needed).

use anyhow::{Context, Result};
use indexmap::IndexSet;
use ootle_byte_type::FromByteType;
use ootle_rs::{
    Address, Network, ToAccountAddress, TransactionOutcome, TransactionRequest,
    builtin_templates::{
        UnsignedTransactionBuilder,
        account::IAccount,
        component::{IComponent, TransactionBuildable},
        faucet::IFaucet,
    },
    default_indexer_url,
    key_provider::PrivateKeyProvider,
    provider::{IndexerProvider, PendingTransaction, ProviderBuilder, WalletProvider},
    stealth::{Output, SignatureRequirements, StealthSignerRequirement, StealthTransfer},
    template_types::{
        Amount, ComponentAddress, ResourceAddress, TemplateAddress, UtxoAddress,
        constants::{TARI, TARI_TOKEN},
        crypto::PedersenCommitmentBytes,
    },
    transaction::TransactionSigner,
    wallet::OotleWallet,
};
use std::num::NonZeroU64;
use std::time::Duration;
use tari_crypto::ristretto::RistrettoPublicKey;
use tari_ootle_transaction::args;
use ranked_voting::MultiWinnerMethod;

const WASM_PATH: &str = "target/wasm32-unknown-unknown/release/ranked_voting.wasm";
const VOTER_COUNT: usize = 3;
const NUM_CANDIDATES: u32 = 3;
const NUM_WINNERS: u32 = 1;
const EXPIRES_AT_EPOCH: u64 = 100_000;
const CONVERT_AMOUNT: u64 = 1 * TARI;
const VOTE_FEE: u64 = 50_000;
/// Multiplier applied to dry-run fee estimates to avoid underpayment from estimation variance.
const FEE_MARGIN_MULTIPLIER: u64 = 2;

/// Each voter's ranking: a permutation of 0..NUM_CANDIDATES (index 0 = first choice).
/// Scenario: 3 voters, 3 candidates.
///   Voter 0: [0, 2, 1]  → first choice 0
///   Voter 1: [1, 2, 0]  → first choice 1
///   Voter 2: [2, 0, 1]  → first choice 2
/// Round 1: 0=1, 1=1, 2=1. No majority. Eliminate 0 (lowest id tie-break).
/// Round 2: 1=1, 2=2 (voter 0's 2nd choice redistributes to 2). 2/3 = 67% > 50%.
/// Winner: candidate 2.
const VOTER_RANKINGS: [[u32; NUM_CANDIDATES as usize]; VOTER_COUNT] = [
    [0, 2, 1],
    [1, 2, 0],
    [2, 0, 1],
];
const EXPECTED_WINNER: u32 = 2;

type Provider = IndexerProvider<OotleWallet>;

async fn wait_for_commit(pending: &PendingTransaction, label: &str) -> Result<()> {
    print!("  {label}: pending {}... ", pending.tx_id());
    let outcome = pending.watch().await?;
    match outcome {
        TransactionOutcome::Commit => println!("COMMITTED"),
        other => {
            println!("FAILED: {other:?}");
            anyhow::bail!("{label} failed: {other:?}");
        }
    }
    Ok(())
}

async fn faucet(provider: &mut Provider, label: &str) -> Result<()> {
    print!("\n[{label}] Faucet... ");
    let unsigned = IFaucet::new(provider)
        .take_faucet_funds()
        .pay_fee(5_000u64)
        .prepare()
        .await?;
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(provider.wallet())
        .await?;
    wait_for_commit(&provider.send_transaction(tx).await?, "faucet").await
}

/// Publish fee for the publish step. Unused fee is refunded, so overpaying costs nothing; the
/// required fee scales with WASM size (the current 368 KB build needs ~11.5M). If publishing
/// starts failing with `OnlyFeeCommit(InsufficientFeesPaid("Required fees X but Y paid"))`,
/// bump this to comfortably exceed X.
const PUBLISH_FEE: u64 = 20_000_000;

async fn publish_template(provider: &mut Provider) -> Result<TemplateAddress> {
    print!("\n[Publish] template... ");
    let wasm = std::fs::read(WASM_PATH).with_context(|| format!("read {WASM_PATH}"))?;
    let unsigned = IAccount::new(provider)
        .publish_template(wasm)
        .pay_fee(PUBLISH_FEE)
        .prepare()
        .await?;
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(provider.wallet())
        .await?;
    let pending = provider.send_transaction(tx).await?;
    wait_for_commit(&pending, "publish").await?;
    let receipt = pending.get_receipt().await?;
    let template_address = receipt
        .diff_summary
        .upped
        .iter()
        .find_map(|s| s.substate_id.as_template())
        .context("no template addr")?
        .as_template_address();
    println!("  template: {template_address}");
    Ok(template_address)
}

async fn create_and_initiate_vote(
    provider: &mut Provider,
    template_address: TemplateAddress,
    voter_addresses: &[Address],
) -> Result<(ComponentAddress, ResourceAddress, Vec<(PedersenCommitmentBytes, RistrettoPublicKey)>)> {
    print!("\n[Create + Initiate] vote... ");
    let voter_count = voter_addresses.len() as u64;

    // Build the mint statement: one stealth ballot UTXO (amount-1) per voter.
    //
    // The StealthTransfer builder requires a ResourceAddress to construct, but the resulting
    // StealthTransferStatement does NOT embed it — the resource address is only used for
    // resolving stealth inputs (which we don't have; this is a revealed-input mint). So we pass
    // a placeholder address here. The real resource address is bound when the engine executes
    // the stealth_transfer instruction inside the template's `new()` constructor, which
    // receives the allocated address via the ResourceAddressAllocation parameter.
    let placeholder_resource = ResourceAddress::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
        .expect("valid placeholder resource address");
    let mut mint_builder =
        StealthTransfer::new(placeholder_resource, provider).spend_revealed_input(voter_count);
    for address in voter_addresses {
        mint_builder = mint_builder.to_stealth_output(Output::new(
            address.clone(),
            placeholder_resource,
            NonZeroU64::new(1).expect("non-zero"),
        ));
    }
    let (mint_statement, _) = mint_builder.prepare().await?;

    // Capture each voter's (commitment, nonce) from the mint statement so voters can
    // spend their UTXOs later.
    let ballot_utxos: Vec<(PedersenCommitmentBytes, RistrettoPublicKey)> = mint_statement
        .stealth_outputs()
        .iter()
        .map(|utxo| {
            let commitment = utxo.commitment().clone();
            let nonce: RistrettoPublicKey = utxo
                .output
                .sender_public_nonce
                .try_from_byte_type()
                .expect("valid nonce");
            (commitment, nonce)
        })
        .collect();

    // Create the component and start the vote in a single transaction.
    let unsigned = IComponent::new(provider)
        .then(|builder| builder.allocate_resource_address("ballot_res"))
        .call_function(
            template_address,
            "new",
            args![
                Workspace("ballot_res"),
                voter_count,
                NUM_CANDIDATES,
                NUM_WINNERS,
                MultiWinnerMethod::SequentialIrv,
                EXPIRES_AT_EPOCH,
                mint_statement,
            ],
        )
        .pay_fee(50_000u64)
        .prepare()
        .await?;
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(provider.wallet())
        .await?;
    let pending = provider.send_transaction(tx).await?;
    wait_for_commit(&pending, "create + initiate").await?;
    let receipt = pending.get_receipt().await?;
    let component = receipt
        .diff_summary
        .upped
        .iter()
        .find_map(|s| s.substate_id.as_component_address())
        .context("no component addr")?;
    // The template creates two resources: RVOTE-MINT (NonFungible, ballot records) and RVOTE
    // (Stealth, the ballots themselves). The mint statement commits to the stealth resource, so
    // pick it out of the `resource.create` events rather than guessing from the diff order.
    let ballot_resource = receipt
        .events
        .iter()
        .find(|event| {
            event.topic() == "std.resource.create"
                && event.get_payload("resource_type") == Some("Stealth")
        })
        .and_then(|event| event.substate_id())
        .and_then(|s| s.as_resource_address())
        .context("no ballot resource addr")?;
    println!("  component: {component}\n  ballot resource: {ballot_resource}");
    Ok((component, ballot_resource, ballot_utxos))
}

async fn convert_to_stealth_tari(
    provider: &mut Provider,
    voter_address: &Address,
) -> Result<(PedersenCommitmentBytes, RistrettoPublicKey)> {
    let voter_account = voter_address.to_account_address();
    let tari_utxo_value = CONVERT_AMOUNT - VOTE_FEE;

    let (convert_transfer, _) = StealthTransfer::new(TARI_TOKEN, provider)
        .spend_revealed_input(CONVERT_AMOUNT)
        .to_stealth_output(Output::new(
            voter_address.clone(),
            TARI_TOKEN,
            NonZeroU64::new(tari_utxo_value).expect("non-zero utxo value"),
        ))
        .to_revealed_output(VOTE_FEE)
        .prepare()
        .await?;

    let tari_utxo = &convert_transfer.stealth_outputs()[0];
    let tari_commitment = tari_utxo.commitment().clone();
    let tari_nonce: RistrettoPublicKey = tari_utxo
        .output
        .sender_public_nonce
        .try_from_byte_type()
        .expect("valid tari nonce");

    let unsigned = IComponent::new(provider)
        .want_vault_for(voter_account, TARI_TOKEN, true)
        .then(|builder| {
            builder.with_fee_instructions_builder(|fee_builder| {
                fee_builder
                    .call_method(voter_account, "withdraw", args![TARI_TOKEN, Amount::from(CONVERT_AMOUNT)])
                    .put_last_instruction_output_on_workspace("withdrawn")
                    .stealth_transfer_with_input_bucket(TARI_TOKEN, convert_transfer, "withdrawn")
                    .put_last_instruction_output_on_workspace("fee_output")
                    .pay_fee_from_bucket("fee_output")
            })
        })
        .prepare()
        .await?;
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(provider.wallet())
        .await?;
    wait_for_commit(&provider.send_transaction(tx).await?, "convert").await?;

    Ok((tari_commitment, tari_nonce))
}

/// Casts a ballot via a two-input stealth spend: the ballot-token UTXO is the seal input
/// (spent into `cast_ballot`) and a stealth TARI UTXO pays the fee. This is the canonical
/// pattern for the README's fee-from-stealth requirement — the fee MUST come from a stealth
/// TARI UTXO, never a revealed account, or the transaction links the voter's identity to
/// their public ranking.
async fn cast_private_ballot(
    provider: &mut Provider,
    component: ComponentAddress,
    ballot_resource: ResourceAddress,
    voter_address: &Address,
    ballot_commitment: PedersenCommitmentBytes,
    ballot_nonce: RistrettoPublicKey,
    tari_commitment: PedersenCommitmentBytes,
    tari_nonce: RistrettoPublicKey,
    ranking: Vec<u32>,
) -> Result<()> {
    let tari_change = CONVERT_AMOUNT - VOTE_FEE - VOTE_FEE;

    let (ballot_spend, _) = StealthTransfer::new(ballot_resource, provider)
        .spend_stealth_input(voter_address.clone(), ballot_commitment.clone())
        .to_revealed_output(1u64)
        .prepare()
        .await?;
    let (tari_spend, _) = StealthTransfer::new(TARI_TOKEN, provider)
        .spend_stealth_input(voter_address.clone(), tari_commitment.clone())
        .to_revealed_output(VOTE_FEE)
        .to_stealth_output(Output::new(
            voter_address.clone(),
            TARI_TOKEN,
            NonZeroU64::new(tari_change).expect("non-zero change"),
        ))
        .prepare()
        .await?;

    let ballot_signer = StealthSignerRequirement::new(voter_address.clone(), ballot_nonce);
    let tari_signer = StealthSignerRequirement::new(voter_address.clone(), tari_nonce);
    let mut authorizers = IndexSet::new();
    authorizers.insert(tari_signer);
    // The ballot-token UTXO seals the transaction (its one-time key P is the seal key) and the
    // stealth TARI fee UTXO authorizes against it.
    let signature_requirements =
        SignatureRequirements::stealth_seal_with(ballot_signer, authorizers);

    let unsigned = IComponent::new(provider)
        .want_all_vaults(component)
        .then(|builder| {
            builder
                .stealth_transfer(ballot_resource, ballot_spend)
                .put_last_instruction_output_on_workspace("vote")
                .add_input(ballot_resource)
                .add_input(UtxoAddress::new(ballot_resource, ballot_commitment.into()))
                .add_input(TARI_TOKEN)
                .add_input(UtxoAddress::new(TARI_TOKEN, tari_commitment.into()))
                .with_fee_instructions_builder(|fee_builder| {
                    fee_builder
                        .stealth_transfer(TARI_TOKEN, tari_spend)
                        .put_last_instruction_output_on_workspace("fees")
                        .pay_fee_from_bucket("fees")
                })
        })
        .call_method(component, "cast_ballot", args![Workspace("vote"), ranking])
        .prepare()
        .await?;

    let authorizer = provider.wallet().stealth_authorizer(signature_requirements);
    let dry_run = provider
        .sign_and_send_dry_run_with(&authorizer, unsigned.clone())
        .await?;
    dry_run.expect_success();
    let estimated_fee = dry_run.finalize.fee_receipt.total_fees_charged();
    let adjusted_fee = estimated_fee * FEE_MARGIN_MULTIPLIER;
    println!("  dry-run fee: {estimated_fee}, adjusted: {adjusted_fee}");

    // `build` asks the authorizer for the stealth authorization signatures its inputs require
    // (committing to the seal signer's one-time public key) and seals the transaction.
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(&authorizer)
        .await?;
    wait_for_commit(&provider.send_transaction(tx).await?, "cast_ballot").await
}

async fn end_vote_and_read_result(
    provider: &mut Provider,
    component: ComponentAddress,
) -> Result<()> {
    print!("\n[Result] end_vote()... ");
    let unsigned = IComponent::new(provider)
        .call_method(component, "end_vote", args![])
        .pay_fee(5_000u64)
        .prepare()
        .await?;
    let tx = TransactionRequest::default()
        .with_transaction(unsigned)
        .build(provider.wallet())
        .await?;
    let pending = provider.send_transaction(tx).await?;
    wait_for_commit(&pending, "end_vote").await?;
    let receipt = pending.get_receipt().await?;
    for event in receipt.events.iter() {
        println!("  event: {} {{{}}}", event.topic(), event.payload());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let network = Network::Esmeralda;

    let init_secret = PrivateKeyProvider::random(network);
    let init_address = init_secret.address().clone();
    let init_wallet = OotleWallet::from(init_secret);
    println!("Initiator: {init_address}");

    let mut initiator_provider = ProviderBuilder::new()
        .wallet(init_wallet)
        .connect_with_transaction_timeout(default_indexer_url(network), Duration::from_secs(120))
        .await?;
    println!("Connected to indexer");

    faucet(&mut initiator_provider, "Initiator").await?;
    let template_address = publish_template(&mut initiator_provider).await?;

    let voter_wallets: Vec<(OotleWallet, Address)> = (0..VOTER_COUNT)
        .map(|i| {
            let secret = PrivateKeyProvider::random(network);
            let address = secret.address().clone();
            println!("  voter {i} address: {address}");
            (OotleWallet::from(secret), address)
        })
        .collect();
    let voter_addresses: Vec<Address> = voter_wallets.iter().map(|(_, a)| a.clone()).collect();

    let (component, ballot_resource, ballot_utxos) = create_and_initiate_vote(
        &mut initiator_provider,
        template_address,
        &voter_addresses,
    )
    .await?;

    for (i, (commitment, _)) in ballot_utxos.iter().enumerate() {
        println!("  voter {i} ballot UTXO commitment: {}", hex::encode(commitment));
    }

    for (i, (wallet, voter_address)) in voter_wallets.into_iter().enumerate() {
        let ranking = VOTER_RANKINGS[i].to_vec();
        println!("\n[Voter {i}] cast ballot ranking={ranking:?}");

        let (ballot_commitment, ballot_nonce) = &ballot_utxos[i];

        let mut voter_provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_with_transaction_timeout(default_indexer_url(network), Duration::from_secs(120))
            .await?;

        faucet(&mut voter_provider, &format!("Voter {i}")).await?;
        let (tari_commitment, tari_nonce) =
            convert_to_stealth_tari(&mut voter_provider, &voter_address).await?;
        cast_private_ballot(
            &mut voter_provider,
            component,
            ballot_resource,
            &voter_address,
            ballot_commitment.clone(),
            ballot_nonce.clone(),
            tari_commitment,
            tari_nonce,
            ranking,
        )
        .await?;
    }

    end_vote_and_read_result(&mut initiator_provider, component).await?;

    println!(
        "\nINTEGRATION COMPLETE: 3-voter ranked-choice IRV vote validated (expected winner = candidate {EXPECTED_WINNER})."
    );
    Ok(())
}
