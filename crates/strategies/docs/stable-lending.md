# stable-lending

**Tier:** official (built in, runs in-process)
**Status:** simulation only. It cannot move real funds in this release.

## Purpose
Each tick, look at the supply APY of the configured stablecoins, pick the best one, and if it is
at or above `min_apy_bps` and the wallet holds at least `chunk`, propose supplying `chunk` of it.
At most one proposal per tick.

## Supported protocols
`aave` (simulated adapter).

## Parameters
| name | default | meaning |
|---|---|---|
| `min_apy_bps` | `300` | do nothing below this APY (basis points) |
| `chunk` | `100` | amount proposed per tick (whole tokens, decimals allowed) |
| `assets` | `USDC,DAI` | stablecoins considered |

## Permissions declared (manifest)
actions `supply, alert`; assets `USDC, DAI`; at most 100 per action and 500 in total.
The risk engine enforces these regardless of what the strategy proposes.

## Assumptions
- All listed assets are priced at about 1 USD and treated as interchangeable.
- The best displayed APY is the best choice. It ignores gas, utilisation changes, caps and fees.

## Risks
- Smart-contract risk, depeg risk and rate changes are not modelled.
- A passing test or a good simulation says nothing about future profitability or safety.
