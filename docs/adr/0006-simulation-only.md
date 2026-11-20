# 0006: Simulation-only and testnet-only until audits

**Status:** Accepted

**Decision.** Only `Network::Simulation` can execute. Mainnet is denied by the risk engine and
the CLI. Testnet is not implemented and says so. No key handling exists. Lifting this requires,
all together: external security review of runtime isolation and signing, independent audit,
legal and operational assessment, and a testnet period (see `docs/roadmap.md`).

**Consequences.** The project can be developed and demonstrated safely. Users must not mistake
simulated results for real-market behavior; every simulate/test output says so.
