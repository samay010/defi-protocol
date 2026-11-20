# 0005: Risk engine as an independent, deny-by-default gate

**Status:** Accepted

**Decision.** All actions, from a strategy or the user, in every mode, pass `RiskPolicy::check`
before anything else. The policy derives from the manifest and is never widened by strategy
output. Order: protocol, action kind, (alerts stop here and are allowed), network, kill switch,
asset, amount, per-action cap, cumulative exposure cap, then (for withdrawals) health factor on
the adapter-simulated post-state. Approvals are re-checked at approval time.

**Consequences.** A buggy or malicious strategy can at worst propose; it cannot exceed what its
manifest declares. Limitation: caps are per instance and enforced locally, there is no portfolio-
wide limit across instances yet, and USD-denominated limits are not implemented (amounts are in
token units).
