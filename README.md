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

Each voter receives exactly one indivisible amount-1 stealth token; the mint-statement invariants (below) force every ballot to be exactly one token at construction. A stealth UTXO can only be spent once — the engine enforces this at the consensus level — so a 1-token ballot cannot be split or cast twice.

### Ballot supply cap (no extra ballots)

The ballot supply is permanently capped at the initial `voter_count`; nobody — including the initiator — can mint additional ballots after the vote starts. The ballot resource's mint rule requires a proof of a **one-of NFT badge** ("RVOTE-MINT") that is created and sealed inside the component during `new()`:

- The badge's own mint/burn/recall rules are `deny_all` with locked updaters, so no second badge can ever exist and the sole copy can never be destroyed or recalled.
- The badge lives in a component vault that no template method exposes, so its proof can never be re-obtained. (Transactions cannot reach a vault directly: the transaction instruction set has no instruction that targets a vault address — see the `Instruction` enum in `tari_ootle_transaction` — so vaults are only reachable from within their owning component's method code.)
- The ballot resource is **ownerless** (`OwnerRule::None`), closing the resource-owner authorization path that would otherwise bypass the mint rule.
- The mint rule's updater is `LOCKED`, so the rule itself can never be changed.

The cap is verifiable by anyone: `voter_count()` returns the number of ballots minted, `ballot_vault_balance()` returns the number cast, and the ballot resource's total supply never exceeds `voter_count`.

### Mint-statement invariants

`new()` verifies three things about the mint statement it receives: its revealed input total equals `voter_count`, it creates exactly `voter_count` stealth outputs — one per voter — and every output promises a minimum value of at least one token. Each output's promise is public, and the engine's range-proof verification binds the output's committed value to be at least its promise; with `voter_count` outputs of at least one token each that sum to exactly `voter_count`, every ballot is forced to be exactly one token at construction. Wrong-valued shapes like `[2,0]` (a 2-token ballot plus a worthless 0-token output) are therefore unconstructible: a 0-value output can only ever be proven with a promise of 0, which the constructor rejects. Ballots can never be burned (`burnable` is `deny_all`), and no further ballots can be minted after the vote starts (see the supply cap above). The mint statement is a public argument of the initiating transaction, so scrutineers can audit exactly what was minted and to which addresses.

One limitation cannot be fixed in the template: nothing on-chain can verify that the minted outputs are distributed to *distinct* voters (two amount-1 ballots could be addressed to the same person, leaving another voter with none). Voter identity and ballot assignment are off-chain; the initiating transaction's public mint statement is the audit point for that.

### Fee-from-stealth requirement (MUST)

For a ballot to be truly unlinkable, the transaction fee must also be paid unlinkably. **Every ballot transaction MUST pay its fee from a stealth TARI UTXO.** Each voter converts revealed TARI into a stealth TARI UTXO first, then pays the ballot transaction's fee from that stealth UTXO (with change returned to another stealth UTXO).

Paying the fee from a revealed source breaks anonymity completely: the fee input links the transaction to the account owner, and because the ballot transaction itself carries the voter's full ranking, that link exposes not only *who voted* but *how they voted*. A revealed fee input effectively defeats the entire stealth mechanism.

The reference client in `client/integration` implements the canonical pattern in `cast_private_ballot`: a two-input stealth spend that uses the ballot-token UTXO as the seal input and a stealth TARI UTXO as the fee input, both bound to the same ephemeral one-time key. Wallet code that builds ballot transactions should follow that pattern exactly — the template cannot enforce it (it never sees fee inputs), so this requirement is a client-side contract.

Fees paid from a bucket (`pay_fee_from_bucket`) are **non-refundable**: the engine takes the revealed fee bucket in full and burns any excess to the fee pool — there is no refund destination that could link a ballot back to a revealed account. The reference client therefore reveals a flat `VOTE_FEE` per ballot that comfortably exceeds the actual fee; the overpay is deliberately uniform so every ballot transaction reveals the same fee.

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
tally/                           Pure tally algorithms (standalone crate `rcv-tally`)
  src/lib.rs                     IRV (always) + STV (`stv` feature) + sequential IRV (`sequential-irv` feature); no template ABI dependency
templates/ranked_voting/         The template (Rust → WASM, pure cdylib)
  src/lib.rs                     Template; re-exports MultiWinnerMethod; wraps tally outputs into ABI result types
  tests/test.rs                  Unit + adversarial + end-to-end in-process tests (feature-dependent, 40 with defaults)
client/integration/             3-voter end-to-end test on the Esmeralda testnet (IRV with redistribution; for primary testing see tests/test.rs, which covers the same scenario in-process)
```

## Build

```bash
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
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting` (default) | IRV + sequential IRV + STV | ~287 KB → ~248 KB (minified) |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features` | IRV only | ~261 KB → ~226 KB (minified) |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features --features stv` | IRV + STV | ~272 KB → ~235 KB (minified) |
| `cargo build --target wasm32-unknown-unknown --release -p ranked_voting --no-default-features --features sequential-irv` | IRV + sequential IRV | ~277 KB → ~240 KB (minified) |

The pure tally algorithms live in the standalone `tally/` crate (`rcv-tally`); the template
depends on it and ships as a **pure `cdylib`**. This matters for size: with a second
crate-type (`rlib`) present, rustc silently drops `-C lto`, and the WASM grows by roughly 20%
(~314 KB vs ~248 KB minified). Tests link `rcv-tally` directly, so keeping `rlib` out costs no
test coverage.

"Minified" is the raw release build run through `wasm-opt -Oz` (see below); exact sizes are
printed by `scripts/minify-wasm.sh`, which fails the run if the minified default build
exceeds 320 KB.

### Publishing

```bash
./scripts/minify-wasm.sh   # builds all 4 combos, writes *.min.wasm artifacts, prints the size table
```

Publish the minified artifact (`target/wasm32-unknown-unknown/release/ranked_voting.default.min.wasm`
for the default build) instead of the raw `.wasm`. The publish fee scales with WASM size —
unused fee is refunded (see `PUBLISH_FEE` in `client/integration/src/main.rs`) — and a smaller
artifact also downloads and instantiates faster for voters.

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

First run `./scripts/minify-wasm.sh` — the client publishes the minified artifact
(`target/wasm32-unknown-unknown/release/ranked_voting.default.min.wasm`).

```bash
cargo run --bin integration
```

This runs a full 3-voter ranked-choice scenario on the Esmeralda testnet:

- Initiator wallet faucets, publishes the template, creates the component with `new(3 candidates, 1 winner, sequential IRV, mint_statement)`, minting 3 stealth ballot UTXOs (one per voter).
- Three voter wallets each faucet, convert TARI to a stealth UTXO for fees, then cast a private ballot via a two-input stealth spend (ballot UTXO → `cast_ballot`, TARI UTXO → fee) with their ranking.
- `end_vote()` returns the IRV result: **candidate 2 wins in 2 rounds** (no first-round majority → candidate 0 eliminated → ballot redistributes to candidate 2 → majority).

## Stealth auth signatures commit to the sealing one-time key

The private two-input ballot spend relies on **stealth authorization signatures committing to the seal signer's one-time key (P)**, not the account key (K): the engine verifies every authorization against `seal_signature().public_key()`, which for a stealth-sealed transaction is the one-time key derived from the seal input's sender nonce. Binding authorizations to K instead produces an "Invalid transaction signature" as soon as a second stealth input needs an authorization — invisible with a single stealth input (no authorizations are produced), which is why it went unnoticed upstream.

An earlier fix for this, [tari-project/tari-ootle#2390](https://github.com/tari-project/tari-ootle/pull/2390), was superseded by a fuller rework ([#2403](https://github.com/tari-project/tari-ootle/pull/2403), released in `ootle-rs` 0.18.0): signature requirements now resolve the seal source once (`SignatureRequirements::stealth_seal_with(seal_signer, authorizers)` — the ballot UTXO seals, the TARI fee UTXO authorizes), the seal key is derivable before signing, and `TransactionRequest::build()` asks the authorizer for the authorizations its inputs require before sealing. No patching or vendoring is needed — the crates.io dependencies carry the fix.
