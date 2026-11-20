# Governance, licensing and attribution

The brief asks for these to be **kept distinct**. They are separate things here.

| Concern | Decision | Where |
|---|---|---|
| Copyright | Contributors keep copyright in their contributions; the project's copyright notices name "the contributors". | file headers / git history |
| Open-source license | **Apache-2.0** (patent grant and clear contribution terms suit infrastructure that others build on) | `LICENSE` |
| Contribution rights | Inbound = outbound: contributions are licensed under Apache-2.0 (Apache-2.0 section 5). Add a DCO sign-off (`Signed-off-by`) requirement when external contributions start. | this doc |
| Trademark | The founding lab may keep its own name and brand. Trademarks are **not** granted by the license (Apache-2.0 section 6). A trademark policy is to be written before the name is widely used. | to do |
| Creator attribution | Permanent historical credit to the founding developer and research lab: kept in `NOTICE`, which Apache-2.0 requires redistributors to preserve. | `NOTICE` |
| Protocol governance | Founders do not keep permanent unilateral control. Early phase: maintainers decide in the open through ADRs and review. Long term: community-driven governance with written rules. No token or DAO unless a requirement clearly justifies it. | ADR process |
| Infrastructure operations | There is no operated infrastructure today (local-first). If independent operators appear, no single privileged operator may be required for the protocol to function. | ADR 0002 |

## How attribution reads

```
Created by:      Founding Research Lab (original creators)
Original project: official project repository
Development:     open-source contributors
Execution:       users' own machines / independent operators (future)
Governance:      maintainers now, community-driven over time
```

Replace the placeholders with the real lab name and repository URL when they exist. The lab's name
is intentionally not invented here.

## Practical next steps (not done in this release)

1. Name the lab/organization in `NOTICE` and the README.
2. Choose and document a DCO or CLA decision.
3. Write a short trademark policy.
4. Publish a governance document: who can merge, how ADRs are accepted, how maintainers are added
   and removed, how the verified-strategy review process becomes multi-party.

This document is engineering guidance, not legal advice. Have a lawyer review licensing and
trademark choices before launch.
