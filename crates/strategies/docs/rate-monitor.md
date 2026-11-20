# rate-monitor

**Tier:** official. **Permissions:** alert only, cannot move funds.

## Purpose
Log the supply APY of every market each tick and alert when any market moves by at least
`min_change_bps` basis points versus the previous tick. The previous values are kept in the
strategy state, so they survive restarts.

## Parameters
| name | default |
|---|---|
| `min_change_bps` | `25` |

## Assumptions
APY from the adapter snapshot is the figure of interest. The first tick only records a baseline.

## Risks
Short-lived spikes can look like trends. This is analytics, not a signal to trade.
