# CLI reference

`protocol [--home DIR] [--sandbox require|off] <command> [options]`. `--sandbox require` (default) runs external strategies in a bubblewrap sandbox and refuses to run them if it is unavailable; `off` runs them with your full user access. Exit codes: `0` success, `1` failure (including
failed tests or a failed instance), `2` usage error. `protocol help` (or `--help` on any command) prints the generated help. Argument parsing uses `clap` (see ADR 0010).
Command names follow the brief's illustrative list; this is the first CLI design and may change
before 1.0.

| Command | What it does |
|---|---|
| `strategies list` | Official strategies and local ones in `./custom-strategies` with trust tier |
| `strategies info <name\|dir>` | Permissions, parameters, trust tier, content hash, docs (built-ins) |
| `create <name> --lang L [--dir D]` | Scaffold an alert-only strategy (default dir `./custom-strategies`) |
| `build <name\|dir>` | Compile or check using the language's fixed build command, in the sandbox (network on for dependencies, only the project directory writable); `--sandbox off` to opt out |
| `test <name\|dir> [--ticks N]` | Run in `calm` and `stress` scenarios (default 12 ticks); fails on errors |
| `simulate <name\|dir> [--ticks N] [--scenario S] [--mode M]` | In-memory run, prints each tick, saves nothing |
| `verify <dir> [--pin]` | Print content hash and permissions; `--pin` records the exact source as reviewed |
| `run <name\|dir> [--mode M] [--ticks N] [--interval-ms MS] [--scenario S] [--instance NAME] [--max-failures N]` | Run continuously (`--ticks 0` = until stopped). Default mode: `approval`. Refused if the instance is already running; continues from saved state after a crash |
| `ps` | List instances. `stale` means the recorded run's process is gone (crashed or killed) |
| `inspect <instance>` | Status, account, health factor, pending actions, strategy state, recent audit log |
| `pause` / `resume` / `stop <instance>` | Lifecycle control (works from another terminal while it runs). `stop` on a stale instance clears it; `pause`/`resume` refuse on a dead run |
| `pending <instance>` | Actions waiting for approval |
| `approve <instance> <id>` / `reject <instance> <id>` | Decide a queued action (risk is re-checked on approve) |
| `act <instance> --kind K --asset A --amount X [--protocol P]` | Manual action, risk-checked like any other |
| `kill-switch on\|off\|status` | Block all fund-moving actions immediately; alerts still work |
| `watch <strategy> --account <0xaddress> [--ticks N] [--interval-ms MS] [--on interval\|block\|event] [--timeout-secs S] [--protocol aave\|compound\|morpho]` | Run a strategy on LIVE read-only Sepolia data (Aave by default; `--protocol compound` or `morpho` read that protocol's market with the environment variables of `compound account` / `morpho account`; those two support `--on interval` and `--on block` only, and Morpho totals are loan-token units, not dollars). Manual mode: fund-moving proposals are only suggested, never executed; nothing is saved. The strategy sees an aggregate view (pseudo-asset `USD`: total collateral and debt, no per-token rates); its health factor follows Aave's definition. Needs the same two environment variables as `aave account`. Interval is at least 1000 ms. `--on block` ticks on each new block and `--on event` only when the account has an Aave Pool event (supply, withdraw, borrow, repay, liquidation; read with `eth_getLogs` over plain HTTP; the first tick runs at once); the events are printed before the tick. `--interval-ms` is then the polling period and waiting stops after `--timeout-secs` (default 900) without a trigger. |
| `aave preflight <supply\|withdraw\|borrow\|repay> --asset <0xtoken> --amount <n> --account <0xaddress>` | Simulate the action with `eth_call` as if the account sent it, at the latest block: prints WOULD SUCCEED or WOULD REVERT with the reason (exit 1 on revert). Nothing is signed or sent; no key is used. The asset address comes from Aave's official address book; decimals are read from the token. Same two environment variables as `aave account`. Not a guarantee and not advice. |
| `aave account <0xaddress>` | Read-only: an Aave V3 account's collateral, debt and health factor on Sepolia. Needs `PROTOCOL_RPC_URL` (https URL of a Sepolia node; usually contains an API key, so env only) and `PROTOCOL_AAVE_POOL` (Pool address from Aave's official address book). One `eth_call`; nothing is signed or sent; other chains, mainnet included, are refused. |
| `compound account <0xaddress>` | Read-only: an account's balances, collateral, debt, borrow capacity and health factor in one Compound III (Comet) market on Sepolia. Needs `PROTOCOL_RPC_URL` and `PROTOCOL_COMPOUND_COMET` (the market address from Compound's own deployment list). Only `eth_call`s; nothing is signed or sent; mainnet and other chains are refused. It applies Compound's liquidation rule itself and compares the result with the contract's `isLiquidatable`, noting any disagreement (the health factor is slightly conservative: debt includes interest up to now). |
| `morpho account <0xaddress>` | Read-only: an account's collateral, debt, max borrow and health factor in one Morpho Blue market on Sepolia. Needs `PROTOCOL_RPC_URL`, `PROTOCOL_MORPHO_ADDRESS` (Morpho Blue address from Morpho's own deployment list) and `PROTOCOL_MORPHO_MARKET_ID` (0x + 64 hex digits). Only `eth_call`s; nothing is signed or sent; mainnet and other chains are refused. Amounts are in the loan token, not dollars. Morpho's views do not accrue interest, so the debt is as of the last market update and the health factor can be slightly optimistic. |

Modes: `autonomous` (execute what passes risk), `approval` (queue for the user), `manual`
(strategy suggestions only; you act with `act`). Scenarios: `calm`, `stress` (WETH price falls
5% of start price per tick, floored at -60%, so the demo account's health factor crosses 1.5
at tick 8, 1.2 at tick 11 and 1.0 at tick 12).

`--network testnet|mainnet` is accepted only to be refused with an explanation.

## Trust tiers

`official` (built in), `verified (pinned)` (exact name + version + source hash was pinned),
`UNVERIFIED` (everything else, including a pinned strategy whose source changed). Unverified
strategies print a warning on `info`, `simulate` and `run`.

## Home directory

`--home DIR`, else `$PROTOCOL_HOME`, else `~/.protocol`. Contains `instances/`, `verified.list`
and the `KILL` marker. Plain text files you can read and back up.
