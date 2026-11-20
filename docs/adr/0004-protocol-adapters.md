# 0004: Protocol adapter interface

**Status:** Accepted

**Decision.** `ProtocolAdapter` (`adapter.rs`) is everything the engine knows about a protocol:
`id`, `snapshot(tick)`, `simulate(tick, action)`, `execute(tick, action)`, `account()`. Snapshots
(`markets`, `account`) use protocol-neutral types. Generic ledger logic (`Snapshot::apply`) lives
in core; protocol-specific rules (e.g. Aave rejecting a withdrawal that liquidates) live in the
adapter's `simulate`. The engine refuses actions for a protocol other than its adapter's.

**Consequences.** Adding Compound or Morpho means a new adapter crate; the engine and strategy
contract do not change. The current adapter is a deterministic *simulation* with synthetic data
and is not real Aave. The interface will likely grow (borrow, rates, positions) as real adapters
are written; changes must stay backward compatible for strategies via the process protocol
version.
