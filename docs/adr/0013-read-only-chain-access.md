# ADR 0013: Read-only chain access, in two layers

Status: Accepted (layer 1 and layer 2 implemented; layer 2 not yet verified against a live testnet)

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
2. **Transport** (`crates/rpc`, crate `protocol-rpc`): an HTTPS JSON-RPC client behind
   `EthCall`, built on `alloy` (the maintainer's choice over a smaller `ureq` + `serde_json`
   client; it brings a much larger dependency tree, which the audit scope must cover). Alloy
   is confined to this one crate and used with `default-features = false` and only `std`,
   `network`, `providers`, `provider-http`, `reqwest-rustls-tls`, `rpc-types-eth`, because its
   default features include a local signer. After every dependency change, CI checks that
   `cargo tree -p protocol-rpc` contains no crate that can hold a private key
   (`alloy-signer-local`, keystore, mnemonic, hardware or cloud signers). The `alloy-signer`
   *trait* crate and `k256` are unavoidably present (alloy-network, signature types), so the
   guarantee is about our code and API surface (no key exists, only `eth_call` is exposed), not
   that signing-capable library code is absent from the build. The crate connects, verifies the chain id is on the
   testnet allowlist (Sepolia only), and runs `eth_call` with a 15 s timeout; it is the only
   code with network access. The RPC URL comes from `PROTOCOL_RPC_URL`, must be https (http only
   to localhost), and is scrubbed from every error message.

Pool addresses are never baked in. The user supplies one (config/env), copied from Aave's official
address book for the chosen testnet; with none configured the protocol stays on the simulation.

## Consequences
- Layer 1 is fully tested offline (no RPC needed) and adds nothing to the supply chain.
- Scales were confirmed on 2026-11-24 from Aave's own sources: `healthFactor` has 18 decimals
  and is `type(uint256).max` with no debt (`GenericLogic.calculateUserAccountData`,
  aave-v3-origin); base-currency amounts are USD with 8 decimals (Sepolia `AaveOracle`:
  `BASE_CURRENCY_UNIT() = 1e8`, `BASE_CURRENCY()` = zero address). `ltv` and
  `currentLiquidationThreshold` are treated as basis points; to be matched against the Aave UI
  with a real testnet position (also the first test of non-zero values).
- No EIP-55 checksum validation (needs Keccak); addresses are checked for shape only.
- Using the result for decisions is a later step and stays behind the risk engine.
- Alloy 2.x needs a recent Rust (MSRV 1.94.1 per its docs); the workspace uses `stable`.
- The transport has no automated test against a live node (CI has no RPC). The manual check is
  `cargo run -p protocol-rpc --example account`, compared with the Aave UI. Pure parts (URL
  check, URL redaction, chain allowlist, ABI) are unit-tested in `adapter-aave`.
- **Accepted advisory RUSTSEC-2026-0387** (`crypto-bigint 0.5.5`, timing of constant-time ops):
  reached only through alloy's `elliptic-curve`/`k256`, which cannot be upgraded yet
  (`elliptic-curve 0.13.x` requires `crypto-bigint ^0.5`; fix is in 0.7.6). It matters only when
  secret integers are processed; this program holds none. Ignored by ID in `deny.toml` and
  `.cargo/audit.toml` with a removal condition: drop both when alloy moves off it.
- Added permissive licenses to `deny.toml` (ISC, BSD-3-Clause, MIT-0, CDLA-Permissive-2.0, Zlib),
  all from the TLS stack and small utility crates; copyleft (LGPL) is not allowed.

## Live read-only mode (`protocol watch`)
`AaveChainRead` (adapter-aave) feeds live Sepolia data to a strategy without any way to act:
`execute` and every non-alert `simulate` are errors, `protocol watch` forces manual mode (fund
proposals are only suggested) and the engine runs with `Network::Testnet`, where execution is
"not implemented". `getUserAccountData` returns totals only, so the strategy sees an aggregate
view with one pseudo-asset `USD` (collateral, debt, average liquidation threshold). Aave defines
health factor as collateral x average threshold / debt, so the strategy's health factor matches
the protocol's up to micro-USD rounding. Strategies that need per-token rates or prices see no
such data in this mode. Nothing is saved (no instance, no store changes). The poll interval is at
least 1 s.

## Preflight simulation (`protocol aave preflight`)
The same read-only transport also supports `eth_call` with a `from` address
(`EthCall::eth_call_from`). `adapter-aave/src/simulate.rs` encodes `supply`, `withdraw`, `borrow`
and `repay` (signatures and parameter order from the official Aave docs, selectors recomputed
with an independent Keccak-256, `interestRateMode = 2`, `referralCode = 0`) and asks the node to
execute them against the latest block. A revert is a normal answer ("would revert" plus the
decoded reason), a failure to ask is an error. Amounts are parsed exactly from the token's own
`decimals()` and never rounded. Asset addresses are never built in. This still cannot sign or
send: the only capability is `eth_call`. It reflects the account's real balances and allowances,
so a supply from an account without tokens or allowance reverts, which is the true answer. It
says nothing about gas, later state, or whether an action is wise.

