use tari_template_lib::prelude::*;

/// Placeholder initiator public keys. Replace these with the real RistrettoPublicKeyBytes of the
/// addresses allowed to initiate a vote before publishing. Any one of them may start a vote.
const INITIATOR_1: RistrettoPublicKeyBytes = RistrettoPublicKeyBytes::zero();
const INITIATOR_2: RistrettoPublicKeyBytes = RistrettoPublicKeyBytes::zero();

/// Returns the access rule that gates initiator-only methods.
fn initiator_rule() -> AccessRule {
    rule!(any_of(public_key(INITIATOR_1), public_key(INITIATOR_2)))
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

/// Confidential ranked-choice voting template (instant-runoff / IRV).
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
#[template]
mod ranked_voting {
    use super::*;
    use super::irv::run_irv;
    use std::collections::{BTreeMap, BTreeSet};

    pub struct RankedVote {
        ballot_resource: ResourceAddress,
        /// Persistent sink for spent ballot tokens. Its balance equals the number of ballots cast
        /// (each ballot is an indivisible amount-1 token), providing a trustless cross-check of
        /// `ballots.len()`.
        ballot_vault: Vault,
        ballots: Vec<Vec<u32>>,
        num_candidates: u32,
        active: bool,
    }

    /// Result of an instant-runoff tally.
    pub struct IrvResult {
        /// The winning candidate id, or `None` if no winner could be determined.
        pub winner: Option<u32>,
        /// Per-round tally: counts of first-preference-among-active candidates, and which
        /// candidate was eliminated that round (`None` on the final, deciding round).
        pub rounds: Vec<RoundTally>,
    }

    /// One round of the instant-runoff tally.
    pub struct RoundTally {
        /// First-preference counts among still-active candidates in this round.
        pub counts: BTreeMap<u32, u64>,
        /// The candidate eliminated at the end of this round, or `None` on the deciding round.
        pub eliminated: Option<u32>,
    }

    impl RankedVote {
        /// Constructor. Creates the stealth ballot resource and the empty ballot pool. The
        /// resource address is pre-allocated by the caller so the initiator can build the mint
        /// `StealthTransferStatement` (which references it) before this call finalizes.
        pub fn new(alloc: ResourceAddressAllocation) -> Component<Self> {
            let ballot_resource = ResourceBuilder::stealth()
                .with_token_symbol("RVOTE")
                .with_divisibility(0)
                .mintable(initiator_rule(), LOCKED)
                .burnable(initiator_rule(), LOCKED)
                .with_address_allocation(alloc)
                .build();

            Component::new(Self {
                ballot_resource,
                ballot_vault: Vault::new_empty(ballot_resource),
                ballots: Vec::new(),
                num_candidates: 0,
                active: false,
            })
            .with_access_rules(
                AccessRules::new()
                    // TODO(before publishing): replace allow_all with `initiator_rule()` once the
                    // real initiator public keys are set in INITIATOR_1/INITIATOR_2 above. Kept
                    // allow_all here so the integration test (random wallets) can drive the flow;
                    // voter confidentiality does not depend on this.
                    .method("initiate_vote", rule!(allow_all))
                    .method("end_vote", rule!(allow_all))
                    // cast_ballot / result / ballot_count / resource_address are callable by
                    // anyone; they deliberately do NOT call
                    // CallerContext::transaction_signer_public_key() so that voters' transactions
                    // can be sealed with an ephemeral key (no identity).
                    .method("cast_ballot", rule!(allow_all))
                    .method("result", rule!(allow_all))
                    .method("ballot_count", rule!(allow_all))
                    .method("ballot_vault_balance", rule!(allow_all))
                    .method("resource_address", rule!(allow_all))
                    .default(rule!(deny_all)),
            )
            .create()
        }

        /// The ballot-token resource address (so the initiator can build outputs for it).
        pub fn resource_address(&self) -> ResourceAddress {
            self.ballot_resource
        }

        /// Start a vote. `voter_count` revealed tokens are minted and converted, via the
        /// caller-provided `mint_statement`, into `voter_count` stealth UTXOs — one per voter,
        /// each owned by a one-time key unlinkable to the voter's real public key. The statement
        /// must carry exactly `voter_count` as its revealed input amount and one stealth output
        /// per voter (built off-chain by the initiator's wallet).
        pub fn initiate_vote(
            &mut self,
            voter_count: u64,
            num_candidates: u32,
            mint_statement: StealthTransferStatement,
        ) {
            assert!(!self.active, "A vote is already in progress");
            assert!(voter_count > 0, "voter_count must be positive");
            assert!(num_candidates > 0, "num_candidates must be positive");
            assert_eq!(
                mint_statement.revealed_input_amount(),
                Amount::from(voter_count),
                "mint statement revealed input must equal voter_count",
            );

            self.num_candidates = num_candidates;
            let manager = ResourceManager::get(self.ballot_resource);
            let minted = manager.mint_stealth(Amount::from(voter_count));
            // Convert the revealed mint into per-voter stealth UTXOs. Any revealed output (which
            // there should not be) is dropped — the mint is fully converted to stealth outputs.
            let _revealed_out = manager
                .stealth_transfer_with_opt_input_bucket(mint_statement, Some(minted));

            self.active = true;
            emit_event(
                "VoteStarted",
                metadata![
                    "voter_count" => voter_count.to_string(),
                    "num_candidates" => num_candidates.to_string(),
                ],
            );
        }

        /// Deposit a revealed ballot-token bucket and record the voter's full ranking. `ranking`
        /// must be a permutation of `0..num_candidates`, where `ranking[0]` is the voter's first
        /// choice, `ranking[1]` their second, and so on. This method deliberately does not call
        /// `CallerContext::transaction_signer_public_key()` so the ballot transaction can be
        /// sealed with an ephemeral one-time key (no voter identity).
        pub fn cast_ballot(&mut self, bucket: Bucket, ranking: Vec<u32>) {
            assert!(self.active, "No active vote");
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
            emit_event("BallotCast", metadata!["ballots" => self.ballots.len().to_string()]);
        }

        /// Number of ballots cast so far.
        pub fn ballot_count(&self) -> u64 {
            self.ballots.len() as u64
        }

        /// Balance of the ballot pool vault (equals ballot_count — a trustless cross-check).
        pub fn ballot_vault_balance(&self) -> Amount {
            self.ballot_vault.balance()
        }

        /// Compute the instant-runoff result over all cast ballots. Read-only and deterministic,
        /// so the outcome is trustless. Returns the winner (if any) and the per-round tally.
        pub fn result(&self) -> IrvResult {
            let (winner, rounds) = run_irv(&self.ballots, self.num_candidates);
            let rounds: Vec<RoundTally> = rounds
                .into_iter()
                .map(|r| RoundTally {
                    counts: r.counts,
                    eliminated: r.eliminated,
                })
                .collect();
            let res = IrvResult { winner, rounds };
            emit_event(
                "Result",
                metadata![
                    "winner" => match res.winner {
                        Some(w) => w.to_string(),
                        None => "none".to_string(),
                    },
                    "rounds" => res.rounds.len().to_string(),
                ],
            );
            res
        }

        /// End the vote (initiator-only). Locks the vote against further ballots and returns the
        /// final result.
        pub fn end_vote(&mut self) -> IrvResult {
            assert!(self.active, "No active vote");
            self.active = false;
            let res = self.result();
            emit_event(
                "VoteEnded",
                metadata![
                    "winner" => match res.winner {
                        Some(w) => w.to_string(),
                        None => "none".to_string(),
                    },
                ],
            );
            res
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
