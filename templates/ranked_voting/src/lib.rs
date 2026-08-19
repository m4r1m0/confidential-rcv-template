use tari_template_lib::prelude::*;
use tari_template_lib::template_macro_deps::minicbor;

/// The tally algorithm used for multi-winner elections (`num_winners > 1`). Chosen once at
/// contract initialization and stored in the component, so the outcome cannot be picked after
/// the fact based on which method gives a more favorable result.
///
/// Which variants exist in a given build is fixed at compile time by cargo features:
/// `SequentialIrv` is always available, `Stv` only when built with `--features stv`.
///
/// This type lives at crate root (rather than inside the `#[template]` module) because it is a
/// function input argument: the template macro's generated dispatcher decodes arguments at crate
/// scope, where items defined inside the template module cannot be resolved.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, minicbor::Encode, minicbor::Decode, minicbor::CborLen,
)]
pub enum MultiWinnerMethod {
    /// Fill each seat by running single-winner IRV, removing the winner, and repeating.
    #[n(0)]
    SequentialIrv,
    /// Single transferable vote with the Droop quota (proportional representation). Requires
    /// the `stv` feature.
    #[cfg(feature = "stv")]
    #[n(1)]
    Stv,
}

/// Pure instant-runoff tally logic, isolated from the template engine so it can be unit-tested
/// directly without stealth-transfer machinery. Returns simple types (winner + per-round data) so
/// it has no dependency on template ABI traits; the template's `result()` method wraps the output
/// into the ABI-compatible `IrvResult` struct.
pub mod irv {
    use std::collections::{BTreeMap, BTreeSet};

    /// Per-round data: first-preference counts among active candidates, and which candidate was
    /// eliminated that round (`None` on the deciding round).
    pub struct Round {
        pub counts: BTreeMap<u32, u64>,
        pub eliminated: Option<u32>,
    }

    /// Instant-runoff tally over a set of ballots. Each ballot is a permutation of
    /// `0..num_candidates` ordered by preference (index 0 = first choice). Deterministic: ties
    /// for elimination are broken by lowest candidate id, so all validators agree on the outcome.
    ///
    /// Returns `(winner, rounds)` where `winner` is `Some(candidate_id)` or `None`, and `rounds`
    /// is the per-round tally trace.
    pub fn run_irv(ballots: &[Vec<u32>], num_candidates: u32) -> (Option<u32>, Vec<Round>) {
        let mut active: BTreeSet<u32> = (0..num_candidates).collect();
        let mut rounds: Vec<Round> = Vec::new();

        loop {
            // Count each ballot's highest-ranked still-active candidate.
            let mut counts: BTreeMap<u32, u64> = active.iter().map(|&c| (c, 0u64)).collect();
            let mut total: u64 = 0;
            for ballot in ballots {
                for &c in ballot {
                    if active.contains(&c) {
                        *counts.get_mut(&c).expect("active candidate counted") += 1;
                        total += 1;
                        break;
                    }
                }
            }

            // Majority: strictly more than half of continuing ballots.
            let mut majority_winner: Option<u32> = None;
            if total > 0 {
                for (&c, &v) in &counts {
                    if v * 2 > total {
                        majority_winner = Some(c);
                        break;
                    }
                }
            }
            if let Some(w) = majority_winner {
                rounds.push(Round {
                    counts,
                    eliminated: None,
                });
                return (Some(w), rounds);
            }

            // If only one active candidate remains, they win (even with zero continuing ballots).
            if active.len() <= 1 {
                rounds.push(Round {
                    counts,
                    eliminated: None,
                });
                return (active.iter().next().copied(), rounds);
            }

            // Eliminate the lowest-count candidate; ties broken by lowest id (BTreeSet order).
            let min_count = *counts.values().min().expect("non-empty active set");
            let to_eliminate = active
                .iter()
                .copied()
                .find(|c| counts.get(c) == Some(&min_count))
                .expect("an elimination candidate exists");
            active.remove(&to_eliminate);
            rounds.push(Round {
                counts,
                eliminated: Some(to_eliminate),
            });
        }
    }
}

/// Pure single-transferable-vote (STV) tally logic for multi-winner elections, isolated from the
/// template engine so it can be unit-tested directly. Returns simple types (winners + per-round
/// data) with no dependency on template ABI traits.
///
/// This module is compiled only when the `stv` feature is enabled (off by default in a
/// `--no-default-features` build). The `stv` variant of `MultiWinnerMethod` / `VoteResult` in the
/// template module mirrors this gate. The IRV module (`pub mod irv`) is always compiled.
#[cfg(feature = "stv")]
pub mod stv {
    use std::collections::{BTreeMap, BTreeSet};

    /// Scale factor for fractional vote values. STV surplus transfers require fractional votes
    /// (a candidate's surplus is distributed proportionally to their voters' next preferences).
    /// We use fixed-point arithmetic with this scale to stay integer-only and deterministic for
    /// consensus. 10000 gives 4 decimal places of precision.
    const VOTE_SCALE: u64 = 10_000;

    /// One ballot's weighted vote. Each ballot starts with weight 1.0 (= VOTE_SCALE) and its
    /// weight is reduced fractionally when its chosen candidate is elected with a surplus.
    struct WeightedBallot {
        ranking: Vec<u32>,
        weight: u64,
    }

    /// Per-round data for the STV tally trace.
    pub struct Round {
        /// Vote counts (scaled) for each still-active candidate in this round.
        pub counts: BTreeMap<u32, u64>,
        /// Candidates elected in this round (reached the quota).
        pub elected: Vec<u32>,
        /// The candidate eliminated in this round, if any.
        pub eliminated: Option<u32>,
        /// The quota threshold used this round.
        pub quota: u64,
    }

    /// Single-transferable-vote tally with the Droop quota. Each ballot is a permutation of
    /// `0..num_candidates` ordered by preference. Returns `(winners, rounds)`.
    ///
    /// Algorithm:
    /// 1. Compute the Droop quota: `floor(continuing_ballots / (num_winners + 1)) + 1`.
    /// 2. Count each ballot's highest-ranked still-active candidate, weighted by the ballot's
    ///    current fractional weight.
    /// 3. Any candidate reaching the quota is elected. Their surplus votes (count - quota) are
    ///    transferred to those ballots' next preferences, with each ballot's weight scaled by
    ///    `surplus / count`.
    /// 4. If no candidate reaches the quota, eliminate the lowest-count candidate (ties broken
    ///    by lowest candidate id for determinism). Their ballots transfer at full weight to their
    ///    next preference.
    /// 5. Repeat until all seats are filled or all remaining candidates fill the remaining seats.
    ///
    /// Deterministic: all validators agree on the outcome.
    pub fn run_stv(
        ballots: &[Vec<u32>],
        num_candidates: u32,
        num_winners: u32,
    ) -> (Vec<u32>, Vec<Round>) {
        let mut active: BTreeSet<u32> = (0..num_candidates).collect();
        let mut elected: Vec<u32> = Vec::new();
        let mut rounds: Vec<Round> = Vec::new();

        // Each ballot starts with full weight (1.0 in scaled fixed-point).
        let mut weighted_ballots: Vec<WeightedBallot> = ballots
            .iter()
            .map(|ranking| WeightedBallot {
                ranking: ranking.clone(),
                weight: VOTE_SCALE,
            })
            .collect();

        loop {
            // All seats filled — done.
            if elected.len() >= num_winners as usize {
                break;
            }

            // Remaining active candidates all get seats (fewer candidates than remaining seats).
            if active.len() <= num_winners as usize - elected.len() {
                for &candidate in active.iter() {
                    elected.push(candidate);
                }
                break;
            }

            // Count weighted votes for each active candidate.
            let mut counts: BTreeMap<u32, u64> = active.iter().map(|&c| (c, 0u64)).collect();
            let mut total_continuing: u64 = 0;
            for ballot in &weighted_ballots {
                for &candidate in &ballot.ranking {
                    if active.contains(&candidate) {
                        *counts
                            .get_mut(&candidate)
                            .expect("active candidate counted") += ballot.weight;
                        total_continuing += ballot.weight;
                        break;
                    }
                }
            }

            // Droop quota: floor(continuing / (seats + 1)) + 1, in scaled units.
            let remaining_seats = num_winners as u64 - elected.len() as u64;
            let quota = if total_continuing > 0 {
                total_continuing / (remaining_seats + 1) + 1
            } else {
                0
            };

            // Check for candidates reaching the quota.
            let newly_elected: Vec<u32> = active
                .iter()
                .copied()
                .filter(|&candidate| counts.get(&candidate).copied().unwrap_or(0) >= quota)
                .collect();

            if !newly_elected.is_empty() {
                // Elect all candidates who reached the quota this round.
                for &candidate in &newly_elected {
                    elected.push(candidate);
                    active.remove(&candidate);
                }

                // Transfer surplus from each newly-elected candidate to those ballots' next
                // preferences. Each ballot assigned to an elected candidate has its weight
                // scaled by surplus / count (the transfer fraction).
                for &candidate in &newly_elected {
                    let candidate_count = counts.get(&candidate).copied().unwrap_or(0);
                    if candidate_count == 0 || quota == 0 {
                        continue;
                    }
                    let surplus = candidate_count - quota;

                    for ballot in &mut weighted_ballots {
                        // Find if this ballot's top active preference was the elected candidate.
                        let top_choice = ballot.ranking.iter().copied().find(|c| {
                            // The candidate is no longer in active (we removed them), so check
                            // if they were the ballot's highest-ranked among the pre-removal set.
                            // We check against the elected candidate directly.
                            *c == candidate
                        });
                        if top_choice.is_some() {
                            // Scale this ballot's weight by surplus / count.
                            ballot.weight = ballot.weight * surplus / candidate_count;
                        }
                    }
                }

                rounds.push(Round {
                    counts,
                    elected: newly_elected,
                    eliminated: None,
                    quota,
                });
                continue;
            }

            // No candidate reached quota — eliminate the lowest-count candidate.
            // Ties broken by lowest candidate id (BTreeSet iteration order).
            let min_count = counts.values().min().copied().unwrap_or(0);
            let to_eliminate = active
                .iter()
                .copied()
                .find(|candidate| counts.get(candidate).copied().unwrap_or(0) == min_count)
                .expect("an elimination candidate exists when active set is non-empty");

            active.remove(&to_eliminate);

            // Eliminated candidate's ballots transfer at full weight to their next preference.
            // No weight change needed — the next count round will pick up the next preference
            // automatically since the eliminated candidate is no longer active.

            rounds.push(Round {
                counts,
                elected: Vec::new(),
                eliminated: Some(to_eliminate),
                quota,
            });
        }

        (elected, rounds)
    }
}

/// Pure sequential-IRV tally logic for multi-winner elections, isolated from the template engine
/// so it can be unit-tested directly.
///
/// Sequential IRV runs single-winner IRV to fill the first seat, removes the winner from all
/// ballots, then runs IRV again on the remaining candidates to fill the second seat, and so on
/// until all seats are filled. It is simpler than STV (no quotas, no surplus transfer, no
/// fractional weights) and reuses the existing `run_irv` function directly.
///
/// This is the **default multi-winner method** when `num_winners > 1`. STV remains available
/// (with the `stv` feature) for those who prefer proportional representation, but sequential IRV
/// is simpler and easier to audit.
///
/// This module is compiled only when the `sequential-irv` feature is enabled (the default; off in
/// a `--no-default-features` build). The `SequentialIrv` variant of `MultiWinnerMethod` /
/// `VoteResult` in the template module mirrors this gate: the variant always exists so the enum
/// is never empty, but `new` rejects it when this module is not compiled in.
#[cfg(feature = "sequential-irv")]
pub mod sequential_irv {
    use super::irv::{Round as IrvRound, run_irv};

    /// One seat's election: the winner (if any) and the IRV sub-rounds that elected them.
    pub struct Seat {
        /// The candidate who won this seat, or `None` if no winner could be determined.
        pub winner: Option<u32>,
        /// The per-round IRV tally trace for this seat's election.
        pub irv_rounds: Vec<IrvRound>,
    }

    /// Sequential-IRV tally over a set of ballots. Each ballot is a permutation of
    /// `0..num_candidates` ordered by preference (index 0 = first choice).
    ///
    /// For each seat: run `run_irv` on the current ballots (with previously-elected candidates
    /// removed and remaining candidates reindexed to 0..N), record the winner, map it back to
    /// the original candidate id, then remove the winner from all ballots for the next seat.
    /// Ties for elimination are broken by lowest candidate id (inherited from `run_irv`),
    /// so all validators agree on the outcome.
    ///
    /// Returns `(winners, seats)` where `winners` is the list of elected candidates in order and
    /// `seats` is the per-seat tally trace.
    pub fn run_sequential_irv(
        ballots: &[Vec<u32>],
        num_candidates: u32,
        num_winners: u32,
    ) -> (Vec<u32>, Vec<Seat>) {
        let mut winners: Vec<u32> = Vec::new();
        let mut seats: Vec<Seat> = Vec::new();

        let mut current_ballots: Vec<Vec<u32>> = ballots.iter().map(|b| b.to_vec()).collect();

        for _seat_index in 0..num_winners {
            // Candidates still in contention: everyone except those already elected.
            let remaining_candidates: Vec<u32> = (0..num_candidates)
                .filter(|candidate| !winners.contains(candidate))
                .collect();

            if remaining_candidates.is_empty() {
                seats.push(Seat {
                    winner: None,
                    irv_rounds: Vec::new(),
                });
                continue;
            }

            // Build a mapping from original candidate id → reindexed id (0..N) for run_irv.
            let original_to_reindexed: std::collections::BTreeMap<u32, u32> = remaining_candidates
                .iter()
                .enumerate()
                .map(|(reindexed, &original)| (original, reindexed as u32))
                .collect();
            let reindexed_to_original: std::collections::BTreeMap<u32, u32> = original_to_reindexed
                .iter()
                .map(|(&original, &reindexed)| (reindexed, original))
                .collect();

            // Reindex each ballot's candidates to the 0..N range, preserving preference order.
            let reindexed_ballots: Vec<Vec<u32>> = current_ballots
                .iter()
                .map(|ballot| {
                    ballot
                        .iter()
                        .copied()
                        .filter_map(|candidate| original_to_reindexed.get(&candidate).copied())
                        .collect()
                })
                .collect();

            let remaining_count = remaining_candidates.len() as u32;
            let (reindexed_winner, irv_rounds) = run_irv(&reindexed_ballots, remaining_count);

            if let Some(reindexed_winner_id) = reindexed_winner {
                let original_id = reindexed_to_original[&reindexed_winner_id];
                winners.push(original_id);

                // Remove the winner from current_ballots for the next seat.
                for ballot in &mut current_ballots {
                    ballot.retain(|candidate| *candidate != original_id);
                }

                seats.push(Seat {
                    winner: Some(original_id),
                    irv_rounds,
                });
            } else {
                seats.push(Seat {
                    winner: None,
                    irv_rounds,
                });
                break;
            }
        }

        (winners, seats)
    }
}

///
/// A vote instance mints one unlinkable stealth ballot-token UTXO per eligible voter (built
/// off-chain by the initiator's wallet and passed in as a `StealthTransferStatement`). Each voter
/// spends their UTXO into the ballot pool via `cast_ballot`, attaching a full ranking of the
/// candidates (a permutation of `0..num_candidates`). Because the spend is a stealth transfer
/// sealed with an ephemeral key (fee paid from a stealth TARI UTXO), no on-chain observer can link
/// any vote transaction to a voter. The ranking itself is public on-chain; only voter *identity*
/// is hidden (consistent with the sibling confidential-voting template's privacy model: obscure
/// *who*, not *what*).
///
/// The tally is instant-runoff: count each ballot's highest-ranked still-active candidate; if one
/// exceeds 50% they win; otherwise eliminate the lowest-count candidate (ties broken by lowest
/// candidate id for determinism) and repeat. `result()` is a deterministic computation over the
/// stored ballots, so the outcome is trustless — any validator or off-chain reader computes the
/// same winner.
///
/// Double-voting is impossible: each voter receives exactly one indivisible amount-1 token, and a
/// stealth UTXO can only be spent once.
///
/// The ballot supply is also permanently capped: minting ballot tokens requires a proof of a
/// one-of NFT badge that is sealed inside the component at construction. After the vote starts,
/// nobody — including the initiator — can mint additional ballots. The ballot resource is
/// ownerless (`OwnerRule::None`), so the resource-owner authorization path cannot be used to
/// bypass the mint rule either.
#[template]
pub mod ranked_voting {
    use super::irv::run_irv;
    #[cfg(feature = "sequential-irv")]
    use super::sequential_irv::run_sequential_irv;
    #[cfg(feature = "stv")]
    use super::stv::run_stv;
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    pub struct RankedVote {
        ballot_resource: ResourceAddress,
        /// Resource holding the single one-of mint badge that authorizes minting ballot tokens.
        mint_badge_resource: ResourceAddress,
        /// Sealed vault holding the sole mint badge. The badge's mint/burn/recall rules are
        /// `deny_all` with locked updaters and no template method exposes this vault, so the
        /// ballot supply is permanently capped at `voter_count`.
        mint_badge_vault: Vault,
        /// Persistent sink for spent ballot tokens. Its balance equals the number of ballots cast
        /// (each ballot is an indivisible amount-1 token), providing a trustless cross-check of
        /// `ballots.len()`.
        ballot_vault: Vault,
        ballots: Vec<Vec<u32>>,
        num_candidates: u32,
        /// Number of winners to elect. 1 = single-winner IRV; >1 = multi-winner via the method
        /// chosen in `new` (`MultiWinnerMethod`).
        num_winners: u32,
        /// The tally algorithm used for multi-winner elections (`num_winners > 1`). Pinned once
        /// at construction; the outcome cannot later be picked from whichever method is favorable.
        multi_winner_method: MultiWinnerMethod,
        /// The number of eligible voters: the ballot supply minted at construction. The supply
        /// can never grow after construction (see `mint_badge_vault`).
        voter_count: u64,
        /// The epoch after which no more ballots may be cast. Prevents elections from being held
        /// up indefinitely by voters who never spend their stealth ballot tokens.
        expires_at_epoch: u64,
        active: bool,
    }

    /// The result of a tally, returned by `result()` / `end_vote()` / `end_vote_expired()`.
    ///
    /// The variant is fixed by the election's configuration: `num_winners == 1` always yields
    /// `Irv`, and multi-winner elections yield whichever variant the `MultiWinnerMethod` chosen
    /// at initialization produces. Which multi-winner variants exist mirrors the
    /// `MultiWinnerMethod` enum and therefore the cargo features.
    #[derive(Clone, Debug)]
    pub enum VoteResult {
        /// Single-winner instant-runoff result.
        Irv(IrvResult),
        /// Sequential-IRV multi-winner result (requires the `sequential-irv` feature).
        #[cfg(feature = "sequential-irv")]
        SequentialIrv(SequentialIrvResult),
        /// STV multi-winner result (requires the `stv` feature).
        #[cfg(feature = "stv")]
        Stv(StvResult),
    }

    /// Result of an instant-runoff tally.
    #[derive(Clone, Debug)]
    pub struct IrvResult {
        /// The winning candidate id, or `None` if no winner could be determined.
        pub winner: Option<u32>,
        /// Per-round tally: counts of first-preference-among-active candidates, and which
        /// candidate was eliminated that round (`None` on the final, deciding round).
        pub rounds: Vec<RoundTally>,
    }

    /// One round of the instant-runoff tally.
    #[derive(Clone, Debug)]
    pub struct RoundTally {
        /// First-preference counts among still-active candidates in this round.
        pub counts: BTreeMap<u32, u64>,
        /// The candidate eliminated at the end of this round, or `None` on the deciding round.
        pub eliminated: Option<u32>,
    }

    /// Result of a single-transferable-vote (multi-winner) tally.
    #[derive(Clone, Debug)]
    pub struct StvResult {
        /// The winning candidate ids, in the order they were elected.
        pub winners: Vec<u32>,
        /// Per-round tally trace.
        pub rounds: Vec<StvRoundTally>,
    }

    /// One round of the STV tally.
    #[derive(Clone, Debug)]
    pub struct StvRoundTally {
        /// Vote counts (scaled) for each still-active candidate in this round.
        pub counts: BTreeMap<u32, u64>,
        /// Candidates elected in this round (reached the quota).
        pub elected: Vec<u32>,
        /// The candidate eliminated in this round, if any.
        pub eliminated: Option<u32>,
        /// The Droop quota threshold used this round.
        pub quota: u64,
    }

    /// Result of a sequential-IRV (multi-winner) tally. This is the default multi-winner method.
    #[derive(Clone, Debug)]
    pub struct SequentialIrvResult {
        /// The winning candidate ids, in the order they were elected.
        pub winners: Vec<u32>,
        /// Per-seat tally trace: one entry per seat, containing the winner and the IRV
        /// sub-rounds that elected them.
        pub seats: Vec<SequentialRoundTally>,
    }

    /// One seat's election in the sequential-IRV tally.
    #[derive(Clone, Debug)]
    pub struct SequentialRoundTally {
        /// The candidate who won this seat, or `None` if no winner could be determined.
        pub winner: Option<u32>,
        /// The IRV sub-rounds for this seat's election (same format as `RoundTally`).
        pub irv_rounds: Vec<RoundTally>,
    }

    impl RankedVote {
        /// Constructor — creates the component, the stealth ballot resource, and starts the vote
        /// in a single transaction.
        ///
        /// # Parameters
        ///
        /// - `alloc`: Pre-allocated resource address. The caller allocates this before the
        ///   transaction so the `mint_statement` can reference it. The resource is created inside
        ///   this call with `with_address_allocation(alloc)`.
        /// - `voter_count`: Number of eligible voters. One stealth ballot UTXO is minted per
        ///   voter.
        /// - `num_candidates`: Number of candidates. Each ballot must be a permutation of
        ///   `0..num_candidates`.
        /// - `num_winners`: Number of seats to fill. 1 = single-winner IRV; >1 = multi-winner
        ///   via `multi_winner_method`.
        /// - `multi_winner_method`: The tally algorithm for multi-winner elections
        ///   (`num_winners > 1`). Pinned here and stored in the component, so the outcome cannot
        ///   be picked after the fact based on whichever method gives a favorable result.
        /// - `expires_at_epoch`: Deadline after which no more ballots may be cast. Prevents
        ///   elections from being held up indefinitely by voters who never spend their stealth
        ///   ballot tokens. After expiration, `end_vote_expired()` finalizes the tally with
        ///   whatever ballots were cast.
        /// - `mint_statement`: Built off-chain by the initiator's wallet. Must carry exactly
        ///   `voter_count` as its revealed input amount and exactly `voter_count` stealth
        ///   outputs, one per voter — both asserted below. Individual output amounts are
        ///   confidential (Pedersen commitments), so they cannot be verified here; the
        ///   cast-time `amount == 1` guard is the enforcement point for per-ballot value.
        ///
        /// The caller of `new` is the initiator: before the deadline only they may end the vote;
        /// after the deadline anyone may finalize it. No template fields need to be edited before
        /// publishing — the initiator's key is captured from the transaction here, and the ballot
        /// supply is permanently capped at `voter_count`: the mint rule of the ballot resource
        /// requires a proof of a one-of badge that is sealed in the component by this call, so no
        /// further ballots can ever be minted.
        pub fn new(
            alloc: ResourceAddressAllocation,
            voter_count: u64,
            num_candidates: u32,
            num_winners: u32,
            multi_winner_method: MultiWinnerMethod,
            expires_at_epoch: u64,
            mint_statement: StealthTransferStatement,
        ) -> Component<Self> {
            assert!(voter_count > 0, "voter_count must be positive");
            assert!(num_candidates > 0, "num_candidates must be positive");
            assert!(num_winners > 0, "num_winners must be positive");
            assert!(
                num_winners <= num_candidates,
                "num_winners cannot exceed num_candidates",
            );
            assert_eq!(
                mint_statement.revealed_input_amount(),
                Amount::from(voter_count),
                "mint statement revealed input must equal voter_count",
            );
            // One stealth output per voter (see the `mint_statement` doc comment above).
            assert_eq!(
                mint_statement.stealth_outputs().len() as u64,
                voter_count,
                "mint statement must create one stealth output per voter",
            );
            // A multi-winner method can only be used if this build compiled it in. The
            // `SequentialIrv` variant always exists (so `MultiWinnerMethod` is never empty), but
            // it requires the `sequential-irv` feature; the failure is a runtime abort here
            // rather than a broken build. Single-winner elections (`num_winners == 1`) are
            // unaffected — the method is ignored for them.
            assert!(
                num_winners == 1
                    || !matches!(multi_winner_method, MultiWinnerMethod::SequentialIrv)
                    || cfg!(feature = "sequential-irv"),
                "MultiWinnerMethod::SequentialIrv requires the `sequential-irv` feature (build with `--features sequential-irv`)",
            );

            // The caller of `new` is the initiator: their key gates ending the vote before the
            // deadline. Capturing the key here instead of hard-coding placeholders means nothing
            // needs to be edited before publishing.
            let initiator = CallerContext::transaction_signer_public_key();

            // The ballot resource's mint rule requires a proof of a one-of NFT badge that is
            // sealed in `mint_badge_vault` when the component is created. The badge authorizes
            // the ballot mint inside this constructor only. The badge's mint/burn/recall rules
            // are deny_all with locked updaters (no second badge can ever exist, and the sole
            // copy can never be destroyed or recalled); its withdraw rule must stay allow_all
            // because creating the constructor's proof is authorized by it — but the rule is
            // inert after construction, since transactions cannot address vaults directly and no
            // template method ever exposes `mint_badge_vault`. The ballot resource is ownerless,
            // so the ballot supply is permanently capped at `voter_count`.
            let badge_bucket = ResourceBuilder::non_fungible()
                .with_token_symbol("RVOTE-MINT")
                .with_owner_rule(OwnerRule::None)
                .mintable(rule!(deny_all), LOCKED)
                .burnable(rule!(deny_all), LOCKED)
                .recallable(rule!(deny_all), LOCKED)
                .withdrawable(rule!(allow_all), LOCKED)
                .update_non_fungible_data(rule!(deny_all), LOCKED)
                .initial_supply_with_data(vec![(NonFungibleId::from_u64(0), (&metadata![], &()))]);
            let mint_badge_resource = badge_bucket.resource_address();

            let ballot_resource = ResourceBuilder::stealth()
                .with_token_symbol("RVOTE")
                .with_divisibility(0)
                .with_owner_rule(OwnerRule::None)
                .mintable(rule!(resource(mint_badge_resource)), LOCKED)
                .burnable(rule!(deny_all), LOCKED)
                .with_address_allocation(alloc)
                .build();

            // Mint voter_count revealed tokens and convert them into per-voter stealth UTXOs
            // via the caller-provided mint statement. Any revealed output (which there should
            // not be) is dropped — the mint is fully converted to stealth outputs. The proof
            // is dropped (releasing its lock on the badge) so the badge can be sealed in the
            // component.
            let mint_proof = badge_bucket.create_proof();
            let manager = ResourceManager::get(ballot_resource);
            let minted = manager.mint_stealth(Amount::from(voter_count));
            let _revealed_out =
                manager.stealth_transfer_with_opt_input_bucket(mint_statement, Some(minted));
            mint_proof.drop();

            Component::new(Self {
                ballot_resource,
                mint_badge_resource,
                mint_badge_vault: Vault::from_bucket(badge_bucket),
                ballot_vault: Vault::new_empty(ballot_resource),
                ballots: Vec::new(),
                num_candidates,
                num_winners,
                multi_winner_method,
                voter_count,
                expires_at_epoch,
                active: true,
            })
            .with_access_rules(
                AccessRules::new()
                    // Initiator-only: the caller of `new` is the initiator, and their key (see
                    // above) is the only one that may end a live vote. After the deadline anyone
                    // may finalize via `end_vote_expired`, so an absent initiator cannot hold up
                    // finalization. Voter confidentiality does not depend on this gate — even a
                    // compromised initiator key cannot inflate the ballot supply, which the
                    // sealed mint badge caps.
                    .method("end_vote", rule!(public_key(initiator)))
                    .method("end_vote_expired", rule!(allow_all))
                    // cast_ballot / result / ballot_count / resource_address are callable by
                    // anyone; they deliberately do NOT call
                    // CallerContext::transaction_signer_public_key() so that voters' transactions
                    // can be sealed with an ephemeral key (no identity).
                    .method("cast_ballot", rule!(allow_all))
                    .method("result", rule!(allow_all))
                    .method("ballot_count", rule!(allow_all))
                    .method("ballot_vault_balance", rule!(allow_all))
                    .method("voter_count", rule!(allow_all))
                    .method("resource_address", rule!(allow_all))
                    .default(rule!(deny_all)),
            )
            .create()
        }

        /// The ballot-token resource address (so the initiator can build outputs for it).
        pub fn resource_address(&self) -> ResourceAddress {
            self.ballot_resource
        }

        /// The number of eligible voters: the ballot supply minted at construction. The supply
        /// can never grow after construction (the ballot resource's mint rule requires a proof of
        /// a badge that is sealed in this component), so this is a hard cap on the number of
        /// ballots that can ever be cast.
        pub fn voter_count(&self) -> u64 {
            self.voter_count
        }

        /// Deposit a revealed ballot-token bucket and record the voter's full ranking. `ranking`
        /// must be a permutation of `0..num_candidates`, where `ranking[0]` is the voter's first
        /// choice, `ranking[1]` their second, and so on. This method deliberately does not call
        /// `CallerContext::transaction_signer_public_key()` so the ballot transaction can be
        /// sealed with an ephemeral one-time key (no voter identity).
        pub fn cast_ballot(&mut self, bucket: Bucket, ranking: Vec<u32>) {
            assert!(self.active, "No active vote");
            let current_epoch = Consensus::current_epoch();
            assert!(
                current_epoch <= self.expires_at_epoch,
                "Voting period has expired (current epoch {current_epoch}, deadline {})",
                self.expires_at_epoch,
            );
            assert_eq!(
                bucket.resource_address(),
                self.ballot_resource,
                "bucket must be the ballot resource",
            );
            assert!(
                bucket.amount() == Amount::from(1u64),
                "each ballot must be exactly one token",
            );
            self.validate_ranking(&ranking);

            self.ballots.push(ranking);
            // The bucket is consumed by deposit into the persistent ballot pool. The token is
            // provably spent (single-spend enforced by the engine); the vault balance equals the
            // number of ballots cast.
            self.ballot_vault.deposit(bucket);
            emit_event(
                "BallotCast",
                metadata!["ballots" => self.ballots.len().to_string()],
            );
        }

        /// Number of ballots cast so far.
        pub fn ballot_count(&self) -> u64 {
            self.ballots.len() as u64
        }

        /// Balance of the ballot pool vault (equals ballot_count — a trustless cross-check).
        pub fn ballot_vault_balance(&self) -> Amount {
            self.ballot_vault.balance()
        }

        /// Compute the tally for the configured election: single-winner IRV when
        /// `num_winners == 1`, otherwise the multi-winner method pinned in `new`
        /// (`MultiWinnerMethod`). Read-only and deterministic.
        pub fn result(&self) -> VoteResult {
            if self.num_winners == 1 {
                VoteResult::Irv(self.irv_result())
            } else {
                self.multi_winner_result()
            }
        }

        /// Single-winner instant-runoff tally. Emits a `Result` event.
        fn irv_result(&self) -> IrvResult {
            let (winner, rounds) = run_irv(&self.ballots, self.num_candidates);
            let rounds: Vec<RoundTally> = rounds
                .into_iter()
                .map(|round| RoundTally {
                    counts: round.counts,
                    eliminated: round.eliminated,
                })
                .collect();
            let result = IrvResult { winner, rounds };
            emit_event(
                "Result",
                metadata![
                    "winner" => match result.winner {
                        Some(w) => w.to_string(),
                        None => "none".to_string(),
                    },
                    "rounds" => result.rounds.len().to_string(),
                ],
            );
            result
        }

        /// Multi-winner tally via the method pinned in `new`. Which arms exist mirrors the
        /// `MultiWinnerMethod` variants and therefore the cargo features; `new` rejects methods
        /// whose tally logic is not compiled in.
        fn multi_winner_result(&self) -> VoteResult {
            match self.multi_winner_method {
                #[cfg(feature = "sequential-irv")]
                MultiWinnerMethod::SequentialIrv => {
                    VoteResult::SequentialIrv(self.sequential_irv_result())
                }
                #[cfg(not(feature = "sequential-irv"))]
                MultiWinnerMethod::SequentialIrv => unreachable!(
                    "MultiWinnerMethod::SequentialIrv requires the `sequential-irv` feature; `new` rejects it",
                ),
                #[cfg(feature = "stv")]
                MultiWinnerMethod::Stv => VoteResult::Stv(self.stv_result()),
            }
        }

        /// Sequential-IRV multi-winner tally (requires the `sequential-irv` feature). Emits a
        /// `ResultMulti` event.
        #[cfg(feature = "sequential-irv")]
        fn sequential_irv_result(&self) -> SequentialIrvResult {
            let (winners, seats) =
                run_sequential_irv(&self.ballots, self.num_candidates, self.num_winners);
            let seats: Vec<SequentialRoundTally> = seats
                .into_iter()
                .map(|seat| SequentialRoundTally {
                    winner: seat.winner,
                    irv_rounds: seat
                        .irv_rounds
                        .into_iter()
                        .map(|irv_round| RoundTally {
                            counts: irv_round.counts,
                            eliminated: irv_round.eliminated,
                        })
                        .collect(),
                })
                .collect();
            let result = SequentialIrvResult { winners, seats };
            emit_event(
                "ResultMulti",
                metadata![
                    "winners" => format!("{:?}", result.winners),
                    "seats" => result.seats.len().to_string(),
                ],
            );
            result
        }

        /// STV multi-winner tally (requires the `stv` feature). Emits a `ResultStv` event.
        #[cfg(feature = "stv")]
        fn stv_result(&self) -> StvResult {
            let (winners, rounds) = run_stv(&self.ballots, self.num_candidates, self.num_winners);
            let rounds: Vec<StvRoundTally> = rounds
                .into_iter()
                .map(|round| StvRoundTally {
                    counts: round.counts,
                    elected: round.elected,
                    eliminated: round.eliminated,
                    quota: round.quota,
                })
                .collect();
            let result = StvResult { winners, rounds };
            emit_event(
                "ResultStv",
                metadata![
                    "winners" => format!("{:?}", result.winners),
                    "rounds" => result.rounds.len().to_string(),
                ],
            );
            result
        }

        /// End the vote (initiator-only). Locks the vote against further ballots and returns the
        /// tally for the configured election: single-winner IRV when `num_winners == 1`,
        /// otherwise the multi-winner method pinned in `new`.
        pub fn end_vote(&mut self) -> VoteResult {
            assert!(self.active, "No active vote");
            self.active = false;
            let result = self.result();
            emit_event(
                "VoteEnded",
                metadata!["ballots_cast" => self.ballots.len().to_string()],
            );
            result
        }

        /// End the vote after the voting period has expired (callable by anyone), even if not
        /// all eligible voters cast ballots. This prevents an election from being held up
        /// indefinitely by non-voting participants — or by an initiator who never returns to
        /// finalize it. The tally is computed with whatever ballots were actually cast, using
        /// the same dispatch as `end_vote`.
        pub fn end_vote_expired(&mut self) -> VoteResult {
            assert!(self.active, "No active vote");
            let current_epoch = Consensus::current_epoch();
            assert!(
                current_epoch > self.expires_at_epoch,
                "Voting period has not yet expired (current epoch {current_epoch}, deadline {})",
                self.expires_at_epoch,
            );
            self.active = false;
            let result = self.result();
            emit_event(
                "VoteEndedExpired",
                metadata!["ballots_cast" => self.ballots.len().to_string()],
            );
            result
        }

        /// Asserts `ranking` is a valid permutation of `0..num_candidates`.
        fn validate_ranking(&self, ranking: &[u32]) {
            assert_eq!(
                ranking.len(),
                self.num_candidates as usize,
                "ranking must list every candidate exactly once",
            );
            let mut seen: BTreeSet<u32> = BTreeSet::new();
            for &c in ranking {
                assert!(c < self.num_candidates, "candidate id {c} out of range");
                assert!(seen.insert(c), "candidate {c} ranked twice");
            }
        }
    }
}
