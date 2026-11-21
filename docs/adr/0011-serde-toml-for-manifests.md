# ADR 0011: `serde` + `toml` for strategy manifests

Status: accepted

## Context
The manifest decides what a strategy may do, so its parser is security-relevant. The first
release used a hand-written parser for a TOML *subset* (comma-separated strings for lists).

## Decision
Parse `strategy.toml` with the `toml` crate through `serde` (derive). The struct uses
`deny_unknown_fields`, so unknown keys are errors; duplicate keys and wrong types are rejected
by the parser. Lists are real TOML arrays. Amounts are integers or quoted decimal strings and
floats are rejected, so money never passes through floating point. All semantic checks (name
safety, caps, health-factor floor, limits) stay in our code and are unchanged.

## Consequences
- The syntax layer is a widely used parser instead of ours; the permission logic remains ours
  and remains covered by the unit and robustness tests.
- **Breaking:** old manifests with `protocols = "aave"` must become `protocols = ["aave"]`, and
  numeric limits are no longer quoted. Pre-release, so no migration path is provided.
- Adds `serde`, `toml` and their transitive crates (checked by `cargo deny` / `cargo audit`).
- Error text for syntax problems is now the `toml` crate's, with line and column.
