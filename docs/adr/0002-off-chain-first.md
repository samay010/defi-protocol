# 0002: Off-chain first, no smart contracts, tokens or DAO

**Status:** Accepted

**Context.** The brief lists contracts, on-chain permissions and operator incentives as possible
future pieces, and tells agents to avoid unnecessary blockchain components and not to add a
token or DAO without clear justification. The engine runs on each user's own machine and only
interacts with existing protocols (Aave) through their own contracts. The project collects no
user data or keys.

**Decision.** No `contracts/` directory, no token, no DAO. Decentralization comes from every
user running their own node. The brief's "Decentralized Execution" phase is optional research.

**Consequences.** + Much less attack surface and scope; + no duplicate-execution or malicious-
operator problem when each user runs their own node. - Spending limits are enforced by the local
risk engine, not on-chain. If an Autonomous Mode signing key leaks, the on-chain account has no
permission layer to fall back on. Mitigations: Approval Mode default, key outside the engine
(keychain/hardware wallet), and optionally a small on-chain permission layer (session keys / Safe
module) later if autonomous operation becomes a real requirement.

**Revisit when** always-on execution without the user's machine, or hard on-chain spending limits,
become concrete requirements.
