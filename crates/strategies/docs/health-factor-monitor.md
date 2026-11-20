# health-factor-monitor

**Tier:** official. **Permissions:** alert only, cannot move funds.

## Purpose
Watch the account health factor and raise an alert when it falls below `warn_below` (warning) or
`critical_below` (critical). Alerts fire when the level *changes* (ok -> warning -> critical), not
on every tick, so a long stretch at the same level is quiet. Recovery to ok is also logged.

## Parameters
| name | default |
|---|---|
| `warn_below` | `1.5` |
| `critical_below` | `1.2` |

## Assumptions
Health factor is computed from the adapter snapshot (collateral x liquidation threshold / debt).
With no debt there is nothing to monitor.

## Risks
An alert is information, not protection. In a fast market the health factor can cross 1.0 between
two ticks. Do not rely on this as your only safeguard.
