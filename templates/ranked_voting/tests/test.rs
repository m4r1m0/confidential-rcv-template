use ranked_voting::irv::run_irv;

/// Helper: ballot `[a, b, c]` means a=1st choice, b=2nd, c=3rd.
fn ballot(rank: &[u32]) -> Vec<u32> {
    rank.to_vec()
}

#[test]
fn test_majority_first_round() {
    // 3 candidates, 5 voters. Candidate 0 gets 3/5 = 60% > 50% in round 1.
    let ballots = vec![
        ballot(&[0, 1, 2]),
        ballot(&[0, 1, 2]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 0, 2]),
        ballot(&[2, 0, 1]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(0));
    assert_eq!(rounds.len(), 1); // decided in round 1
    assert_eq!(rounds[0].counts.get(&0), Some(&3));
    assert_eq!(rounds[0].counts.get(&1), Some(&1));
    assert_eq!(rounds[0].counts.get(&2), Some(&1));
    assert!(rounds[0].eliminated.is_none());
}

#[test]
fn test_redistribution_changes_winner() {
    // Classic IRV: candidate with most first-preferences loses after redistribution.
    // 3 candidates, 4 voters.
    //   Voter 1: [0, 2, 1]
    //   Voter 2: [0, 2, 1]
    //   Voter 3: [1, 2, 0]
    //   Voter 4: [2, 1, 0]
    // Round 1: 0=2, 1=1, 2=1. No majority (need >2). Eliminate 1 (tie with 2, lowest id).
    // Round 2: 0=2, 2=2 (voter 3's 2nd choice is 2). No majority (need >2). Eliminate 0 (tie, lowest id).
    // Wait — 0=2 and 2=2, need >2 for majority. 2*2=4 > 4? No. Eliminate 0 (lowest id).
    // Round 3: 2=4. 4*2=8 > 4. Winner = 2.
    let ballots = vec![
        ballot(&[0, 2, 1]),
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[2, 1, 0]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(2));
    assert!(rounds.len() >= 2);
    // Round 1: eliminated 1 (tie 1=1, 2=1, lowest id)
    assert_eq!(rounds[0].eliminated, Some(1));
}

#[test]
fn test_tie_break_lowest_id() {
    // 2 candidates, 2 voters: each gets 1 vote. No majority (1*2=2, not > 2).
    // Eliminate lowest id = 0. Remaining: candidate 1 wins.
    let ballots = vec![ballot(&[0, 1]), ballot(&[1, 0])];
    let (winner, rounds) = run_irv(&ballots, 2);
    assert_eq!(winner, Some(1));
    assert_eq!(rounds[0].eliminated, Some(0)); // 0 eliminated (lowest id tie-break)
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
    assert_eq!(winner, Some(2)); // 1/1 = 100% > 50%
    assert_eq!(rounds.len(), 1);
}

#[test]
fn test_all_eliminated_until_one_remains() {
    // 4 candidates, 4 voters each ranking a different candidate first.
    // Round 1: each gets 1. No majority. Eliminate 0 (lowest id, all tied at 1).
    // Round 2: 1=1, 2=1, 3=1. Eliminate 1.
    // Round 3: 2=1, 3=1. Eliminate 2.
    // Round 4: 3=1. Only one remains. Winner = 3.
    let ballots = vec![
        ballot(&[0, 1, 2, 3]),
        ballot(&[1, 2, 3, 0]),
        ballot(&[2, 3, 0, 1]),
        ballot(&[3, 0, 1, 2]),
    ];
    let (winner, rounds) = run_irv(&ballots, 4);
    assert_eq!(winner, Some(3));
    assert_eq!(rounds.len(), 4); // 3 elimination rounds + 1 final
}

#[test]
fn test_no_ballots() {
    let (winner, rounds) = run_irv(&[], 3);
    // No ballots → total=0 each round → eliminate lowest id until one remains.
    assert_eq!(winner, Some(2)); // last remaining after eliminating 0, 1
    assert!(rounds.len() >= 2);
}

#[test]
fn test_redistribution_to_second_choice() {
    // 3 candidates, 3 voters.
    //   Voter 1: [0, 2, 1]
    //   Voter 2: [1, 2, 0]
    //   Voter 3: [2, 0, 1]
    // Round 1: 0=1, 1=1, 2=1. No majority (need >1.5). Eliminate 0 (lowest id).
    // Round 2: 1=1, 2=2 (voter 1's 2nd choice is 2). 2*2=4 > 3. Winner = 2.
    let ballots = vec![
        ballot(&[0, 2, 1]),
        ballot(&[1, 2, 0]),
        ballot(&[2, 0, 1]),
    ];
    let (winner, rounds) = run_irv(&ballots, 3);
    assert_eq!(winner, Some(2));
    assert_eq!(rounds[0].eliminated, Some(0));
    // In round 2, candidate 2 should have 2 votes (voter 1 redistributed to 2, voter 3 stays 2)
    assert_eq!(rounds[1].counts.get(&2), Some(&2));
}

#[test]
fn test_exactly_fifty_percent_not_majority() {
    // 2 candidates, 4 voters: 2-2 split. Each gets 2, 2*2=4 not > 4. No majority.
    // Eliminate 0 (lowest id). Winner = 1.
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
