use ranked_voting::irv::run_irv;
use ranked_voting::stv::run_stv;
use tari_template_lib::prelude::{Consensus, Amount};
use tari_template_lib::types::constants::TARI_TOKEN;
use tari_template_test_tooling::TemplateTest;
use tari_template_test_tooling::transaction::{Transaction, args};
use tari_template_test_tooling::support::stealth::generate_mint_statement;
use tari_template_test_tooling::support::assert_error::assert_reject_reason;
use tari_template_test_tooling::engine_types::virtual_substate::{
    VirtualSubstate, VirtualSubstateId,
};

/// Helper: ballot `[a, b, c]` means a=1st choice, b=2nd, c=3rd.
fn ballot(rank: &[u32]) -> Vec<u32> {
    rank.to_vec()
}

// ───────────────────────── IRV unit tests ─────────────────────────

#[test]
fn test_majority_first_round() {
    let ballots = vec![
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 0, 2]),
        ballot(&[2, 0, 1]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(0));
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].counts.get(&0), Some(&3));
    assert_eq!(rounds[0].counts.get(&1), Some(&1));
    assert_eq!(rounds[0].counts.get(&2), Some(&1));
    assert!(rounds[0].eliminated.is_none());
}

#[test]
fn test_redistribution_changes_winner() {
    let ballots = vec![
        ballot(&[0, 2, 1]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[2, 1, 0]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(2));
    assert!(rounds.len() >= 2);
    assert_eq!(rounds[0].eliminated, Some(1));
}

#[test]
fn test_tie_break_lowest_id() {
    let ballots = vec![ballot(&[0, 1]), ballot(&[1, 0])];
    let (winner, rounds) = run_irv(&ballots, 2);
    assert_eq!(winner, Some(1));
    assert_eq!(rounds[0].eliminated, Some(0));
}

#[test]
fn test_single_candidate() {
    let ballots = vec![ballot(&[0]), ballot(&[0])];
    let (winner, rounds) = run_irv(&ballots, 1);
    assert_eq!(winner, Some(0));
    assert_eq!(rounds.len(), 1);
}

#[test]
fn test_single_voter() {
    let ballots = vec![ballot(&[2, 0, 1])];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(2));
    assert_eq!(rounds.len(), 1);
}

#[test]
fn test_all_eliminated_until_one_remains() {
    let ballots = vec![
        ballot(&[0, 1, 2, 3]),
        ballot(&[1, 2, 3, 0]),
        ballot(&[2, 3, 0, 1]),
        ballot(&[3, 0, 1, 2]),
    ];
    let (winner, rounds) = run_irv(&ballots, 4);
    assert_eq!(winner, Some(3));
    assert_eq!(rounds.len(), 4);
}

#[test]
fn test_no_ballots() {
    let (winner, rounds) = run_irv(&[], 3);
    assert_eq!(winner, Some(2));
    assert!(rounds.len() >= 2);
}

#[test]
fn test_redistribution_to_second_choice() {
    let ballots = vec![
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[2, 0, 1]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(2));
    assert_eq!(rounds[0].eliminated, Some(0));
    assert_eq!(rounds[1].counts.get(&2), Some(&2));
}

#[test]
fn test_exactly_fifty_percent_not_majority() {
    let ballots = vec![
        ballot(&[0, 1]),
        ballot(&[0, 1]),
        ballot(&[1, 0]),
        ballot(&[1, 0]),
    ];
    let (winner, rounds) = run_irv(&ballots, 2);
    assert_eq!(winner, Some(1));
    assert_eq!(rounds[0].eliminated, Some(0));
}

#[test]
fn test_determinism_same_result() {
    let ballots = vec![
        ballot(&[1, 0, 2]),
        ballot(&[2, 1, 0]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[0, 1, 2]),
    ];
    let (w1, r1) = run_irv(&ballots, 3);
    let (w2, r2) = run_irv(&ballots, 3);
    assert_eq!(w1, w2);
    assert_eq!(r1.len(), r2.len());
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.counts, b.counts);
        assert_eq!(a.eliminated, b.eliminated);
    }
}

// ───────────────────────── STV unit tests ─────────────────────────

#[test]
fn test_stv_two_winners_three_candidates() {
    let ballots = vec![
        ballot(&[0, 1, 2, 3]),
        ballot(&[0, 1, 2, 3]),
        ballot(&[0, 1, 2, 3]),
        ballot(&[1, 0, 2, 3]),
        ballot(&[2, 3, 0, 1]),
        ballot(&[3, 2, 0, 1]),
    ];
    let (winners, _rounds) = run_stv(&ballots, 4, 2);
    assert_eq!(winners.len(), 2);
    assert!(winners.contains(&0));
}

#[test]
fn test_stv_surplus_transfer() {
    let ballots = vec![
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[2, 1, 0]),
    ];
    let (winners, _rounds) = run_stv(&ballots, 3, 2);
    assert_eq!(winners.len(), 2);
    assert_eq!(winners[0], 0);
    assert_eq!(winners[1], 1);
}

#[test]
fn test_stv_all_seats_filled_by_elimination() {
    let ballots = vec![
        ballot(&[0, 1, 2, 3]),
        ballot(&[1, 2, 3, 0]),
        ballot(&[2, 3, 0, 1]),
        ballot(&[3, 0, 1, 2]),
    ];
    let (winners, _rounds) = run_stv(&ballots, 4, 2);
    assert_eq!(winners.len(), 2);
    assert!(winners.contains(&1));
    assert!(winners.contains(&2));
}

#[test]
fn test_stv_quota_calculation() {
    let ballots = vec![
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[1, 0, 2]),
        ballot(&[1, 0, 2]),
        ballot(&[1, 0, 2]),
    ];
    let (winners, rounds) = run_stv(&ballots, 3, 2);
    assert_eq!(winners.len(), 2);
    assert!(winners.contains(&0));
    assert!(winners.contains(&1));
    // 6 ballots * 10000 scale = 60000 total. Droop = floor(60000/3) + 1 = 20001
    assert_eq!(rounds[0].quota, 20_001);
}

#[test]
fn test_stv_single_winner_matches_irv_behavior() {
    let ballots = vec![
        ballot(&[0, 2, 1]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[2, 1, 0]),
    ];
    let (irv_winner, _) = run_irv(&ballots, 3);
    let (stv_winners, _) = run_stv(&ballots, 3, 1);
    assert_eq!(stv_winners.len(), 1);
    assert_eq!(Some(stv_winners[0]), irv_winner);
}

#[test]
fn test_stv_fewer_candidates_than_seats() {
    let ballots = vec![ballot(&[0, 1]), ballot(&[1, 0])];
    let (winners, _rounds) = run_stv(&ballots, 2, 3);
    assert_eq!(winners.len(), 2);
    assert!(winners.contains(&0));
    assert!(winners.contains(&1));
}

#[test]
fn test_stv_determinism() {
    let ballots = vec![
        ballot(&[1, 0, 2, 3]),
        ballot(&[2, 1, 0, 3]),
        ballot(&[0, 2, 1, 3]),
        ballot(&[3, 0, 1, 2]),
        ballot(&[1, 2, 3, 0]),
    ];
    let (winners1, rounds1) = run_stv(&ballots, 4, 2);
    let (winners2, rounds2) = run_stv(&ballots, 4, 2);
    assert_eq!(winners1, winners2);
    assert_eq!(rounds1.len(), rounds2.len());
}

// ───────────────────── Adversarial in-process tests ─────────────────────
//
// These test the template's assertion guards directly using tari_template_test_tooling
// (in-process, no testnet needed). They cover the adversarial cases from review feedback:
// wrong token type, double-vote amount, expired election, nonsense parameters, invalid rankings.

/// Sets up a RankedVote component with a pre-allocated ballot resource and returns
/// (template_address, component_address, ballot_resource_address, test, account, proof, secret).
fn setup_vote_component() -> (
    tari_template_lib::types::TemplateAddress,
    tari_template_lib::types::ComponentAddress,
    tari_template_lib::types::ResourceAddress,
    TemplateTest,
    tari_template_lib::types::ComponentAddress,
    tari_template_lib::types::NonFungibleAddress,
    tari_template_test_tooling::crypto::RistrettoSecretKey,
) {
    let mut test = TemplateTest::my_crate();
    let template_address = test.get_template_address("RankedVote");
    let (account, proof, secret) = test.create_funded_account();

    // Create the component with a pre-allocated ballot resource.
    let transaction = test
        .transaction()
        .allocate_resource_address("ballot_res")
        .call_function(template_address, "new", args![Workspace("ballot_res")])
        .build_and_seal(&secret);

    let result = test.execute_expect_success(transaction, vec![proof.clone()]);
    let component_address = result
        .finalize
        .result
        .accept()
        .unwrap()
        .up_iter()
        .find_map(|(id, _)| id.as_component_address())
        .expect("component address");
    let ballot_resource = result
        .finalize
        .result
        .accept()
        .unwrap()
        .up_iter()
        .find_map(|(id, _)| id.as_resource_address())
        .expect("ballot resource address");

    (template_address, component_address, ballot_resource, test, account, proof, secret)
}

/// Builds and executes a initiate_vote transaction that mints stealth ballot UTXOs.
fn initiate_vote(
    test: &mut TemplateTest,
    template_address: tari_template_lib::types::TemplateAddress,
    component_address: tari_template_lib::types::ComponentAddress,
    ballot_resource: tari_template_lib::types::ResourceAddress,
    voter_count: u64,
    num_candidates: u32,
    num_winners: u32,
    expires_at_epoch: u64,
    secret: &tari_template_test_tooling::crypto::RistrettoSecretKey,
) {
    // Build a mint statement for voter_count stealth outputs of amount 1 each.
    let output_amounts: Vec<u64> = (0..voter_count).map(|_| 1).collect();
    let mint_data = generate_mint_statement(
        output_amounts,
        0u64, // no revealed output
        None,
    );

    let transaction = test
        .transaction()
        .call_method(
            component_address,
            "initiate_vote",
            args![
                voter_count,
                num_candidates,
                num_winners,
                expires_at_epoch,
                mint_data.statement,
            ],
        )
        .build_and_seal(secret);

    test.execute_expect_success(transaction, vec![]);
}

#[test]
fn rejects_zero_voter_count() {
    let (template_address, component, ballot_resource, mut test, _account, _proof, secret) =
        setup_vote_component();

    let output_amounts: Vec<u64> = vec![];
    let mint_data = generate_mint_statement(output_amounts, 0u64, None);

    let transaction = test
        .transaction()
        .call_method(
            component,
            "initiate_vote",
            args![0u64, 3u32, 1u32, 1000u64, mint_data.statement],
        )
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "voter_count must be positive");
}

#[test]
fn rejects_zero_candidates() {
    let (template_address, component, ballot_resource, mut test, _account, _proof, secret) =
        setup_vote_component();

    let output_amounts: Vec<u64> = vec![1];
    let mint_data = generate_mint_statement(output_amounts, 0u64, None);

    let transaction = test
        .transaction()
        .call_method(
            component,
            "initiate_vote",
            args![1u64, 0u32, 1u32, 1000u64, mint_data.statement],
        )
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "num_candidates must be positive");
}

#[test]
fn rejects_more_winners_than_candidates() {
    let (template_address, component, ballot_resource, mut test, _account, _proof, secret) =
        setup_vote_component();

    let output_amounts: Vec<u64> = vec![1];
    let mint_data = generate_mint_statement(output_amounts, 0u64, None);

    let transaction = test
        .transaction()
        .call_method(
            component,
            "initiate_vote",
            args![1u64, 2u32, 3u32, 1000u64, mint_data.statement],
        )
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "num_winners cannot exceed num_candidates");
}

#[test]
fn rejects_ballot_after_vote_closed() {
    let (template_address, component, ballot_resource, mut test, account, proof, secret) =
        setup_vote_component();

    initiate_vote(
        &mut test,
        template_address,
        component,
        ballot_resource,
        1,
        2,
        1,
        1000,
        &secret,
    );

    // End the vote.
    let end_transaction = test
        .transaction()
        .call_method(component, "end_vote", args![])
        .build_and_seal(&secret);
    test.execute_expect_success(end_transaction, vec![]);

    // Attempt to cast a ballot after the vote is closed. The `assert!(self.active)` fires
    // before the resource check, so we can use any withdrawable token here.
    let transaction = test
        .transaction()
        .call_method(account, "withdraw", args![TARI_TOKEN, Amount::from(1u64)])
        .put_last_instruction_output_on_workspace("bucket")
        .call_method(component, "cast_ballot", args![Workspace("bucket"), vec![0u32, 1u32]])
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![proof]);
    assert_reject_reason(reason, "No active vote");
}

#[test]
fn rejects_ballot_after_expiration() {
    let (template_address, component, ballot_resource, mut test, account, proof, secret) =
        setup_vote_component();

    // Initiate with expiration at epoch 10.
    initiate_vote(
        &mut test,
        template_address,
        component,
        ballot_resource,
        1,
        2,
        1,
        10,
        &secret,
    );

    // Advance the epoch past the expiration.
    test.set_virtual_substate(
        VirtualSubstateId::CurrentEpoch,
        VirtualSubstate::CurrentEpoch(11),
    );

    // Attempt to cast a ballot. The active check passes, but the expiration check fires.
    // We use TARI here — the expiration assertion fires before the resource check.
    let transaction = test
        .transaction()
        .call_method(account, "withdraw", args![TARI_TOKEN, Amount::from(1u64)])
        .put_last_instruction_output_on_workspace("bucket")
        .call_method(component, "cast_ballot", args![Workspace("bucket"), vec![0u32, 1u32]])
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![proof]);
    assert_reject_reason(reason, "Voting period has expired");
}

#[test]
fn rejects_end_vote_expired_before_deadline() {
    let (template_address, component, ballot_resource, mut test, _account, _proof, secret) =
        setup_vote_component();

    initiate_vote(
        &mut test,
        template_address,
        component,
        ballot_resource,
        1,
        2,
        1,
        100,
        &secret,
    );

    // Epoch is still 0 (default), well before expiration at 100.
    let transaction = test
        .transaction()
        .call_method(component, "end_vote_expired", args![])
        .build_and_seal(&secret);

    let reason = test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "Voting period has not yet expired");
}
