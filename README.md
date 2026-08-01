# Confidential Ranked-Choice Voting Template for Tari Ootle

A confidential ranked-choice voting template for the Tari Ootle L2 platform. Voters cast unlinkable ranked ballots using stealth-addressed ballot tokens — no on-chain observer can link any ballot transaction to the voter who cast it. The instant-runoff (IRV) tally is computed on-chain and is trustlessly readable by anyone.

This template extends the [confidential-voting-template](https://github.com/m4r1m0/confidential-voting-template) (yes/no voting) to ranked-choice voting with instant-runoff counting.

## Privacy model

This template inherits the **coinjoin-style blending** design from the sibling yes/no template (obscure *who sent what ballot*, not the ballot content):

1. **Initiator mints stealth ballot tokens.** When a vote is initiated, the template mints one indivisible amount-1 ballot token per eligible voter and converts them into **stealth UTXOs** — each owned by a one-time key unlinkable to the voter's real public key. The stealth outputs are built off-chain by the initiator's wallet and passed to the template as a `StealthTransferStatement`.

2. **Voters spend privately.** Each voter spends their stealth ballot-token UTXO via `cast_ballot`, attaching their full ranking of the candidates. Because the spend is a **stealth transfer sealed with an ephemeral one-time key** (with the transaction fee paid from a separate stealth TARI UTXO), no on-chain observer can link the ballot transaction to a voter identity. The `cast_ballot` method deliberately never calls `CallerContext::transaction_signer_public_key()` so that ephemeral sealing works.

3. **Tally is public, on-chain, and trustless.** Anyone can call `result()` to compute the instant-runoff winner from the stored ballots. The computation is deterministic, so every validator and off-chain reader agrees on the outcome.

### What is private vs. public

| Private (hidden) | Public (on-chain) |
|---|---|
| Voter identity (who cast which ballot) | Ballot content (each voter's full ranking) |
| | Number of ballots cast |
| | The IRV winner and per-round tallies |

This is consistent with the sibling template's model: voter *anonymity* is protected by stealth-address unlinkability; ballot *content* (the ranking) is visible on-chain. The ranking must be public for the on-chain IRV computation to be trustless.

### Double-vote prevention

Each voter receives exactly one indivisible amount-1 stealth token. A stealth UTXO can only be spent once — the engine enforces this at the consensus level. There is no way to split the token or spend it twice.

### Fee-from-stealth requirement

For a ballot to be truly unlinkable, the transaction fee must also be paid unlinkably. Each voter converts revealed TARI into a stealth TARI UTXO first, then pays the ballot transaction's fee from that stealth UTXO (with change returned to another stealth UTXO). If the fee were paid from a revealed account instead, the transaction would be linkable to the account owner.

## IRV tally algorithm (single-winner)

The single-winner tally uses **instant-runoff voting (IRV)**:

1. Count each ballot's highest-ranked **still-active** candidate as a vote for that candidate.
2. If any candidate has **strictly more than 50%** of the continuing ballots, they win.
3. Otherwise, **eliminate** the candidate with the fewest votes. Ties are broken by **lowest candidate id** (deterministic, so all validators agree).
4. Repeat until a winner is found or only one candidate remains.

Each ballot is a permutation of `0..num_candidates`, where `ranking[0]` is the voter's first choice, `ranking[1]` their second, and so on. When a voter's top candidate is eliminated, their ballot redistributes to their next-highest-ranked still-active candidate.

The `result()` method is read-only (`&self`) and deterministic, so the outcome is trustless — no trusted tally authority is needed.

## Sequential IRV tally algorithm (multi-winner, default)

For elections with multiple seats (e.g. council elections where seats are fungible), the **default multi-winner method** is sequential IRV:

1. Run single-winner IRV on all candidates to fill the first seat.
2. Remove the winner from all ballots (filter them out, preserving preference order).
3. Reindex remaining candidates to `0..N` and run IRV again to fill the next seat.
4. Repeat until all seats are filled or no candidates remain.

Sequential IRV is simpler than STV (no quotas, no surplus transfer, no fractional weights) and reuses the existing single-winner IRV logic directly. It is not proportional — a majority bloc could win all seats — but it is easy to audit and understand.

Use `result_multi()` / `end_vote_multi()` when `num_winners > 1`. The logic is in a self-contained `pub mod sequential_irv` — to strip it, delete that module and remove the `result_multi` / `end_vote_multi` / `end_vote_expired_multi` methods.

## STV tally algorithm (multi-winner, alternative)

For those who prefer proportional representation, the template also supports **single transferable vote (STV)** with the Droop quota as an alternative multi-winner method:

1. Compute the Droop quota: `floor(continuing_ballots / (num_winners + 1)) + 1`.
2. Count each ballot's highest-ranked still-active candidate, weighted by the ballot's current fractional weight (scaled by 10000 for fixed-point precision).
3. Any candidate reaching the quota is elected. Their surplus votes are transferred to those ballots' next preferences, with each ballot's weight scaled by `surplus / count`.
4. If no candidate reaches the quota, eliminate the lowest-count candidate (ties broken by lowest candidate id). Their ballots transfer at full weight to their next preference.
5. Repeat until all seats are filled or all remaining candidates fill the remaining seats.

Use `result_stv()` / `end_vote_stv()` when `num_winners > 1` and you explicitly want STV. The STV logic is in a self-contained `pub mod stv`.

## Election expiration

Elections have an `expires_at_epoch` deadline set at initiation. After the deadline, no more ballots may be cast (`cast_ballot` checks `Consensus::current_epoch()`). This prevents an election from being held up indefinitely by voters who never spend their stealth ballot tokens.

After expiration, `end_vote_expired()` (single-winner), `end_vote_expired_multi()` (multi-winner sequential IRV, default), or `end_vote_expired_stv()` (multi-winner STV) finalizes the tally with whatever ballots were actually cast.

## Template API

| Method | Access | Description |
|---|---|---|
| `new(alloc, voter_count, num_candidates, num_winners, expires_at_epoch, mint_statement)` | — | Constructor. Creates the stealth ballot resource, mints per-voter stealth ballot UTXOs, and starts the vote — all in one transaction. `num_winners=1` → IRV, `>1` → STV. |
| `resource_address()` | allow_all | Returns the ballot-token resource address. |
| `cast_ballot(bucket, ranking)` | allow_all | Deposits one token + records a full ranking. Identity-free. Rejects after expiration. |
| `ballot_count()` | allow_all | Returns the number of ballots cast so far. |
| `ballot_vault_balance()` | allow_all | Returns the ballot pool vault balance (cross-check: equals `ballot_count`). |
| `result()` | allow_all | Computes the single-winner IRV result. Read-only. |
| `result_multi()` | allow_all | Computes the multi-winner sequential-IRV result (default for `num_winners > 1`). Read-only. |
| `result_stv()` | allow_all | Computes the multi-winner STV result (alternative, proportional). Read-only. |
| `end_vote()` | initiator-only | Ends the vote, returns final IRV result, locks further ballots. |
| `end_vote_multi()` | initiator-only | Ends the vote, returns final sequential-IRV result (default multi-winner). |
| `end_vote_stv()` | initiator-only | Ends the vote, returns final STV result (alternative). |
| `end_vote_expired()` | initiator-only | Finalizes an expired election with IRV result (even if not all ballots cast). |
| `end_vote_expired_multi()` | initiator-only | Finalizes an expired election with sequential-IRV result (default multi-winner). |
| `end_vote_expired_stv()` | initiator-only | Finalizes an expired election with STV result (alternative). |

> **Before publishing:** set `INITIATOR_1` / `INITIATOR_2` to the `RistrettoPublicKeyBytes` of the addresses allowed to initiate/end votes, and switch the `end_vote` access rules from `allow_all` to `initiator_rule()`. Voter confidentiality does not depend on this (ballots are identity-free regardless), but without it anyone can end a vote.

## Project layout

```
templates/ranked_voting/         The template (Rust → WASM)
  src/lib.rs                     Template + pure IRV (pub mod irv) + STV (pub mod stv) + sequential IRV (pub mod sequential_irv)
  tests/test.rs                  Unit + adversarial in-process tests (29 tests)
client/integration/              3-voter end-to-end test (IRV with redistribution)
vendor/tari-ootle/               Git submodule: fork of tari-ootle with the two-input signing fix
```

## Build

```bash
# Clone with submodules (includes the patched ootle-rs fork)
git clone --recurse-submodules https://github.com/m4r1m0/confidential-rcv-template.git

# If already cloned, initialize the submodule:
git submodule update --init --recursive

# Compile the template to WASM
cargo build --target wasm32-unknown-unknown --release -p ranked_voting

# Run the IRV algorithm unit tests
cargo test -p ranked_voting

# Build the integration test client
cargo build --bin integration
```

## Run the integration test

```bash
cargo run --bin integration
```

This runs a full 3-voter ranked-choice scenario on the Esmeralda testnet:

- Initiator wallet faucets, publishes the template, creates the component, and calls `initiate_vote(3, 3, mint_statement)` to mint 3 stealth ballot UTXOs (one per voter).
- Three voter wallets each faucet, convert TARI to a stealth UTXO for fees, then cast a private ballot via a two-input stealth spend (ballot UTXO → `cast_ballot`, TARI UTXO → fee) with their ranking.
- `end_vote()` returns the IRV result: **candidate 2 wins in 2 rounds** (no first-round majority → candidate 0 eliminated → ballot redistributes to candidate 2 → majority).

## The ootle-rs signing fix

The `vendor/tari-ootle` submodule is a fork of [tari-ootle](https://github.com/tari-project/tari-ootle) containing a fix for a **two-stealth-input signing bug** in `crates/wallet/ootle-rs/src/wallet/stealth.rs` (`WalletStealthAuthorizer::create_authorizations`).

**The bug:** The engine verifies every authorization signature against the seal signature's public key. For account-sealed transactions that is the account key (K); for stealth-sealed transactions it is the seal signer's *one-time* key (P), **not** the account key. The upstream implementation always bound authorizations to K, which is invisible with a single stealth input (no authorizations needed) but produces an invalid signature as soon as a second stealth input requires an authorization signature.

**The fix:** When the transaction is stealth-sealed (`must_sign_with_account_key == false`), derive the one-time stealth owner public key P via `derive_stealth_owner_public_key(seal_signer.signer(), seal_signer.public_nonce())` and bind authorization signatures to P instead of K.

This fix is what makes the private two-input ballot spend possible: the ballot-token UTXO is the seal input (P_seal), and the TARI fee UTXO is an additional authorization — both signed correctly against the one-time key.

The fix is applied via a `[patch.crates-io]` override in the workspace `Cargo.toml`, pointing at the submodule fork. Once the upstream PR ([tari-project/tari-ootle#2390](https://github.com/tari-project/tari-ootle/pull/2390)) is merged, the patch section and submodule can be removed.
