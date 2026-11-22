# ADR 0013: Read-only chain access, in two layers

Status: Accepted (layer 1 implemented; layer 2 proposed)

## Context
Phase 2 needs real Aave data. The risk is not reading, it is accidentally gaining the ability to
write: signing, sending transactions or holding keys. Official Aave addresses and some scales
(health factor, base currency) could not be confirmed from the documentation pages available when
this was written, and must not be filled in from memory.

## Decision
Split chain access into two layers.

1. **Pure logic, no I/O, no new dependencies** (`protocol-adapter-aave::onchain`): strict address
   parsing, ABI encoding of `getUserAccountData(address)` and decoding of its six return values,
   and an `EthCall` trait with exactly one method, a read-only `eth_call`. There is no way to
   express a transaction or a signature in this API. The 4-byte selector (`0xbf92857c`) was
   recomputed with an independent Keccak-256, and signature and return-value order come from the
   official Aave "Pool" documentation.
2. **Transport** (proposed, next step): an HTTPS JSON-RPC client behind `EthCall`, using
   audited/popular crates (an HTTP client with rustls, and `serde_json`) rather than hand-written
   TLS or JSON. It will get its own dependency review, `cargo deny` and `cargo audit` pass, and it
   will be the only code with network access; strategies stay offline in the sandbox.

Pool addresses are never baked in. The user supplies one (config/env), copied from Aave's official
address book for the chosen testnet; with none configured the protocol stays on the simulation.

## Consequences
- Layer 1 is fully tested offline (no RPC needed) and adds nothing to the supply chain.
- Scales (`HEALTH_FACTOR_SCALE`, base-currency units) are named constants flagged "to confirm"
  against the official source and against the Aave UI on a testnet before any use in decisions.
- No EIP-55 checksum validation (needs Keccak); addresses are checked for shape only.
- Using the result for decisions is a later step and stays behind the risk engine.
