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
| `build <name\|dir>` | Compile or check using the language's fixed build command |
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
