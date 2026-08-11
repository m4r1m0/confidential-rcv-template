# Confidential Ranked-Choice Voting Template for Tari Ootle

A confidential ranked-choice voting template for the Tari Ootle L2 platform. Voters cast unlinkable ranked ballots using stealth-addressed ballot tokens — no on-chain observer can link any ballot transaction to the voter who cast it. The instant-runoff (IRV) tally is computed on-chain and is trustlessly readable by anyone.

## Privacy model

This template inherits the **coinjoin-style blending** design from the sibling yes/no template (obscure *who sent what ballot*, not the ballot content):

1. **Initiator mints stealth ballot tokens.** When a vote is initiated, the template mints one indivisible amount-1 ballot token per eligible voter and converts them into **stealth UTXOs** — each owned by a one-time key unlinkable to the voter's real public key. The stealth outputs are built off-chain by the initiator's wallet and passed to the template as a `StealthTransferStatement`. The supply is permanently capped at `voter_count` (see [Ballot supply cap](#ballot-supply-cap-no-extra-ballots)).

2. **Voters spend privately.** Each voter spends their stealth ballot-token UTXO via `cast_ballot`, attaching their full ranking of the candidates. Because the spend is a **stealth transfer sealed with an ephemeral one-time key** (with the transaction fee paid from a separate stealth TARI UTXO), no on-chain observer can link the ballot transaction to a voter identity. The `cast_ballot` method deliberately never calls `CallerContext::transaction_signer_public_key()` so that ephemeral sealing works.

3. **Tally is public, on-chain, and trustless.** Anyone can call `result()` to compute the outcome from the stored ballots: instant-runoff for single-winner elections, or the multi-winner method chosen at initiation (see below). The computation is deterministic, so every validator and off-chain reader agrees on the outcome.

### What is private vs. public

| Private (hidden) | Public (on-chain) |
|---|---|
| Voter identity (who cast which ballot) | Ballot content (each voter's full ranking) |
| | Number of ballots cast |
| | The IRV winner and per-round tallies |

This is consistent with the sibling template's model: voter *anonymity* is protected by stealth-address unlinkability; ballot *content* (the ranking) is visible on-chain. The ranking must be public for the on-chain IRV computation to be trustless.

### Double-vote prevention

Each voter receives exactly one indivisible amount-1 stealth token. A stealth UTXO can only be spent once — the engine enforces this at the consensus level. There is no way to split the token or spend it twice.

### Ballot supply cap (no extra ballots)

The ballot supply is permanently capped at the initial `voter_count`; nobody — including the initiator — can mint additional ballots after the vote starts. The ballot resource's mint rule requires a proof of a **one-of NFT badge** ("RVOTE-MINT") that is created and sealed inside the component during `new()`:

- The badge's own mint/burn/recall rules are `deny_all` with locked updaters, so no second badge can ever exist and the sole copy can never be destroyed or recalled.
- The badge lives in a component vault that no template method exposes, and transactions cannot address vaults directly, so its proof can never be re-obtained.
- The ballot resource is **ownerless** (`OwnerRule::None`), closing the resource-owner authorization path that would otherwise bypass the mint rule.
- The mint rule's updater is `LOCKED`, so the rule itself can never be changed.

The cap is verifiable by anyone: `voter_count()` returns the number of ballots minted, `ballot_vault_balance()` returns the number cast, and the ballot resource's total supply never exceeds `voter_count`.

### Fee-from-stealth requirement (MUST)

For a ballot to be truly unlinkable, the transaction fee must also be paid unlinkably. **Every ballot transaction MUST pay its fee from a stealth TARI UTXO.** Each voter converts revealed TARI into a stealth TARI UTXO first, then pays the ballot transaction's fee from that stealth UTXO (with change returned to another stealth UTXO).

Paying the fee from a revealed source breaks anonymity completely: the fee input links the transaction to the account owner, and because the ballot transaction itself carries the voter's full ranking, that link exposes not only *who voted* but *how they voted*. A revealed fee input effectively defeats the entire stealth mechanism.

The reference client in `client/integration` implements the canonical pattern in `cast_private_ballot`: a two-input stealth spend that uses the ballot-token UTXO as the seal input and a stealth TARI UTXO as the fee input, both bound to the same ephemeral one-time key. Wallet code that builds ballot transactions should follow that pattern exactly — the template cannot enforce it (it never sees fee inputs), so this requirement is a client-side contract.

## IRV tally algorithm (single-winner)

The single-winner tally uses **instant-runoff voting (IRV)**:

1. Count each ballot's highest-ranked **still-active** candidate as a vote for that candidate.
2. If any candidate has **strictly more than 50%** of the continuing ballots, they win.
3. Otherwise, **eliminate** the candidate with the fewest votes. Ties are broken by **lowest candidate id** (deterministic, so all validators agree).
4. Repeat until a winner is found or only one candidate remains.

Each ballot is a permutation of `0..num_candidates`, where `ranking[0]` is the voter's first choice, `ranking[1]` their second, and so on. When a voter's top candidate is eliminated, their ballot redistributes to their next-highest-ranked still-active candidate.

The `result()` method is read-only (`&self`) and deterministic, so the outcome is trustless — no trusted tally authority is needed.

## Choosing the multi-winner method

A single election either elects one winner (IRV) or several. For multi-winner elections, the tally method is **chosen once, when the vote is created**, by passing a `MultiWinnerMethod` to `new()`:

- `MultiWinnerMethod::SequentialIrv` — the **default**: fill each seat by running single-winner IRV, removing the winner, and repeating. Simpler than STV (no quotas, no surplus transfer, no fractional weights) and reuses the single-winner logic directly. It is not proportional — a majority bloc could win all seats — but it is easy to audit and understand.
- `MultiWinnerMethod::Stv` — **single transferable vote** with the Droop quota for proportional representation.

The choice is stored in the component, so the outcome cannot be picked after the fact based on which method gives a favorable result. `num_winners = 1` always uses plain IRV regardless of the configured method.

## Sequential IRV tally algorithm (multi-winner, default)

For elections with multiple seats (e.g. council elections where seats are fungible), the **default multi-winner method** is sequential IRV:

1. Run single-winner IRV on all candidates to fill the first seat.
2. Remove the winner from all ballots (filter them out, preserving preference order).
3. Reindex remaining candidates to `0..N` and run IRV again to fill the next seat.
4. Repeat until all seats are filled or no candidates remain.

Sequential IRV is simpler than STV (no quotas, no surplus transfer, no fractional weights) and reuses the existing single-winner IRV logic directly. It is not proportional — a majority bloc could win all seats — but it is easy to audit and understand.

## STV tally algorithm (multi-winner, alternative)

For those who prefer proportional representation, the template also supports **single transferable vote (STV)** with the Droop quota as an alternative multi-winner method:

1. Compute the Droop quota: `floor(continuing_ballots / (num_winners + 1)) + 1`.
2. Count each ballot's highest-ranked still-active candidate, weighted by the ballot's current fractional weight (scaled by 10000 for fixed-point precision).
3. Any candidate reaching the quota is elected. Their surplus votes are transferred to those ballots' next preferences, with each ballot's weight scaled by `surplus / count`.
4. If no candidate reaches the quota, eliminate the lowest-count candidate (ties broken by lowest candidate id). Their ballots transfer at full weight to their next preference.
5. Repeat until all seats are filled or all remaining candidates fill the remaining seats.

## Election expiration

Elections have an `expires_at_epoch` deadline set at initiation. After the deadline, no more ballots may be cast (`cast_ballot` checks `Consensus::current_epoch()`). This prevents an election from being held up indefinitely by voters who never spend their stealth ballot tokens.

After expiration, `end_vote_expired()` finalizes the tally with whatever ballots were actually cast. It is callable by anyone, so the election cannot be held up by an initiator who never returns; the initiator can also use it.

## Template API

| Method | Access | Description |
|---|---|---|
| `new(alloc, voter_count, num_candidates, num_winners, multi_winner_method, expires_at_epoch, mint_statement)` | — | Constructor. Creates the stealth ballot resource, mints per-voter stealth ballot UTXOs, seals the mint badge (permanently capping the supply at `voter_count`), and starts the vote — all in one transaction. The caller of `new` is the **initiator**. |
| `resource_address()` | allow_all | Returns the ballot-token resource address. |
| `voter_count()` | allow_all | Returns the number of eligible voters (the ballot supply, which can never grow). |
| `cast_ballot(bucket, ranking)` | allow_all | Deposits one token + records a full ranking. Identity-free. Rejects after expiration. |
| `ballot_count()` | allow_all | Returns the number of ballots cast so far. |
| `ballot_vault_balance()` | allow_all | Returns the ballot pool vault balance (cross-check: equals `ballot_count`). |
| `result()` | allow_all | Computes the tally: IRV when `num_winners = 1`, otherwise the `MultiWinnerMethod` chosen in `new`. Read-only. Returns a `VoteResult`. |
| `end_vote()` | initiator-only | Ends the vote, returns the final `VoteResult`, locks further ballots. |
| `end_vote_expired()` | anyone (after the deadline) | Finalizes an expired election with the final `VoteResult` (even if not all ballots cast). |

The initiator is whoever called `new()` — no keys need to be edited before publishing. Only the initiator can end a live vote; anyone can finalize it once the deadline has passed, so an absent initiator cannot hold up finalization. Voter confidentiality does not depend on this gate (ballots are identity-free regardless); it exists so only the vote's creator can close it early.

## Project layout

```
templates/ranked_voting/         The template (Rust → WASM)
  src/lib.rs                     Template + pure IRV (pub mod irv) + STV (pub mod stv, `stv` feature) + sequential IRV (pub mod sequential_irv, `sequential-irv` feature)
  tests/test.rs                  Unit + adversarial + end-to-end in-process tests (feature-dependent, ~39 with defaults)
client/integration/             3-voter end-to-end test on the Esmeralda testnet (IRV with redistribution; for primary testing see tests/test.rs, which covers the same scenario in-process)
vendor/tari-ootle/               Git submodule: fork of tari-ootle with the two-input signing fix
```

## Build

```bash
# Clone with submodules (includes the patched ootle-rs fork)
git clone --recurse-submodules https://github.com/m4r1m0/confidential-rcv-template.git

# If already cloned, initialize the submodule:
git submodule update --init --recursive

# Compile the template to WASM (all methods)
cargo build --target wasm32-unknown-unknown --release -p ranked_voting

# Run the tests
cargo test -p ranked_voting

# Build the integration test client
cargo build --bin integration
```

### Build options: choosing which tally methods to include

The template is compiled with **cargo features** that select which multi-winner methods are
included in the WASM. A smaller WASM means cheaper deployments and faster code downloads for
voters. The default build includes everything:

| Build command | Included methods | WASM size |
|---|---|---|
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting` (default) | IRV + sequential IRV + STV | ~367 KB |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features` | IRV only | ~341 KB |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features --features stv` | IRV + STV | ~351 KB |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features --features sequential-irv` | IRV + sequential IRV | ~359 KB |

If you strip methods out, the component API still works exactly the same — the only difference
is which `MultiWinnerMethod` values `new()` accepts:

- With **IRV only**, create single-winner elections (`num_winners = 1`) exactly as before.
- With **IRV + STV** (`--no-default-features --features stv`), pass `MultiWinnerMethod::Stv` for
  multi-winner elections.
- With **IRV + sequential IRV** (`--no-default-features --features sequential-irv`) or the
  default build, pass `MultiWinnerMethod::SequentialIrv`.

Using a method that wasn't compiled in fails immediately in `new()` with a clear error message,
so a stripped build can never silently compute the wrong tally.

The matching test command for each option uses the same feature flags, e.g.
`cargo test -p ranked_voting --no-default-features --features stv`.

## Run the integration test

```bash
cargo run --bin integration
```

This runs a full 3-voter ranked-choice scenario on the Esmeralda testnet:

- Initiator wallet faucets, publishes the template, creates the component with `new(3 candidates, 1 winner, sequential IRV, mint_statement)`, minting 3 stealth ballot UTXOs (one per voter).
- Three voter wallets each faucet, convert TARI to a stealth UTXO for fees, then cast a private ballot via a two-input stealth spend (ballot UTXO → `cast_ballot`, TARI UTXO → fee) with their ranking.
- `end_vote()` returns the IRV result: **candidate 2 wins in 2 rounds** (no first-round majority → candidate 0 eliminated → ballot redistributes to candidate 2 → majority).

## The ootle-rs signing fix

The `vendor/tari-ootle` submodule is a fork of [tari-ootle](https://github.com/tari-project/tari-ootle) containing a fix for a **two-stealth-input signing bug** in `crates/wallet/ootle-rs/src/wallet/stealth.rs` (`WalletStealthAuthorizer::create_authorizations`).

**The bug:** The engine verifies every authorization signature against the seal signature's public key. For account-sealed transactions that is the account key (K); for stealth-sealed transactions it is the seal signer's *one-time* key (P), **not** the account key. The upstream implementation always bound authorizations to K, which is invisible with a single stealth input (no authorizations needed) but produces an invalid signature as soon as a second stealth input requires an authorization signature.

**The fix:** When the transaction is stealth-sealed (`must_sign_with_account_key == false`), derive the one-time stealth owner public key P via `derive_stealth_owner_public_key(seal_signer.signer(), seal_signer.public_nonce())` and bind authorization signatures to P instead of K.

This fix is what makes the private two-input ballot spend possible: the ballot-token UTXO is the seal input (P_seal), and the TARI fee UTXO is an additional authorization — both signed correctly against the one-time key.

The fix is applied via a `[patch.crates-io]` override in the workspace `Cargo.toml`, pointing at the submodule fork. Once the upstream PR ([tari-project/tari-ootle#2390](https://github.com/tari-project/tari-ootle/pull/2390)) is merged, the patch section and submodule can be removed.
