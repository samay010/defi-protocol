# Strategy protocol and SDK guide

All languages use the same contract. The engine starts your program once per tick, writes a
request to **stdin**, and reads your reply from **stdout**. Amounts are **integer micro-units**
(1 token = `1000000`). Strategies never sign or send anything: they only *propose*.

## Request (engine -> strategy)

One item per line, ending with `END`:

```
TICK 3
PARAM min_apy_bps 300                 # from [params] in strategy.toml
STATE n 2                             # what you stored with SETSTATE last tick
MARKET USDC apy_bps=391 price_usd_micro=1000000
ACCOUNT supplied WETH 2000000
ACCOUNT borrowed USDC 2000000000
ACCOUNT wallet USDC 1000000000
HEALTH_FACTOR 2.475000                # or: HEALTH_FACTOR none  (no debt)
END
```

## Response (strategy -> engine)

```
LOG <text>                                         free-form log line
PROPOSE <kind> <protocol> <asset|-> <amount_micro> [note...]
SETSTATE <key> <value>                             replaces the whole state; send all keys you want kept
DONE                                               mandatory last line
```

- `kind`: `supply`, `withdraw`, `repay`, `alert` (alert uses asset `-` and amount `0`).
- Parsing is **strict**: an unknown line, a malformed `PROPOSE`, output after `DONE`, or a missing
  `DONE` (a truncated response) makes the tick fail. It is never read as "no proposals".
- Limits: 20 proposals per tick, 64 KiB of output, `tick_timeout_ms` (default 2000, max 60000).
- The process starts in the strategy directory with an **empty environment** (only `PATH`, which is
  the fixed `/usr/local/bin:/usr/bin:/bin`; bare program names are looked up only there). It runs in
  its own process group and on timeout the **whole group is killed**, so background processes you
  fork do not outlive the tick. A CPU-time limit applies (timeout rounded up, plus one second).
  Stderr (first 4 KiB) is shown if you exit non-zero.
- Optional `max_memory_mb` (16 to 1048576) sets an address-space limit. It is off by default; do not
  set it for Java (the JVM reserves far more address space than it uses).

## What happens to a proposal

Risk engine (protocol, action, asset, caps, exposure, health factor, kill switch) -> adapter
dry-run -> risk again on the resulting state -> execute / queue / suggest depending on mode.
Denials are reported with a reason and recorded in the audit log. See `docs/architecture.md`.

## Manifest (`strategy.toml`)

```toml
name = "my-strategy"            # lowercase letters, digits, - or _, starts with a letter
version = "0.1.0"
language = "python"             # rust | c | cpp | python | java
entry = "python3 strategy.py"   # program + args, run inside the strategy directory
description = "What it does"
protocols = ["aave"]
assets = ["USDC", "DAI"]
allowed_actions = ["supply", "alert"]   # default: alert only
max_amount_per_action = 100        # tokens; default 0. Use "100.5" (a string) for decimals
max_total_exposure = 500           # cumulative per instance; default 0
min_health_factor = 1.5          # withdrawals that would go below this are denied
tick_timeout_ms = 2000
max_memory_mb = 256              # optional; omit for no limit (and always omit for Java)

[params]
my_threshold = "42"                # delivered as PARAM lines
```

The manifest is real TOML. Lists are arrays. Amounts are whole numbers or quoted decimal strings, never
floats (money must not pass through floating point). `tick_timeout_ms` and `max_memory_mb` are integers,
`min_health_factor` is a number. Unknown keys, duplicate keys and wrong types are errors. `[params]`
values may be strings, numbers or booleans and are delivered as text.

Declaring a fund-moving action requires `assets` and a non-zero `max_amount_per_action`.

## Per-language quick start

`protocol create <name> --lang <lang>` generates a working skeleton for each. Build commands are
fixed by the CLI (not read from the manifest):

| Language | Entry | `protocol build` runs |
|---|---|---|
| Rust | `./target/release/<name>` | `cargo build --release` |
| Python | `python3 strategy.py` | `python3 -m py_compile strategy.py` |
| C | `./build/strategy` | `cc -O2 -Wall -o build/strategy strategy.c` |
| C++ | `./build/strategy` | `c++ -O2 -Wall -std=c++17 -o build/strategy strategy.cpp` |
| Java | `java -cp build Strategy` | `javac -d build Strategy.java` |

Minimal Python example that supplies 50 USDC when APY is high (needs
`allowed_actions = ["supply", "alert"]`, `assets = ["USDC"]`, and caps in the manifest):

```python
import sys
apy = 0
for raw in sys.stdin:
    p = raw.rstrip("\n").split(" ")
    if p[0] == "MARKET" and p[1] == "USDC":
        apy = int(p[2].split("=")[1])
    elif p[0] == "END":
        break
if apy >= 400:
    print("PROPOSE supply aave USDC 50000000 high yield")
print(f"LOG usdc apy {apy} bps")
print("DONE")
```

## Language-specific notes

- **Rust / C / C++** are candidates for a WASM/WASI runtime later (strong isolation by default).
  Today they run as ordinary processes.
- **Python / Java** need out-of-process runners; real isolation needs OS mechanisms
  (containers, seccomp, namespaces). Not built yet. See `docs/security.md`.
- Do not assume every language can compile to WebAssembly: the process contract exists so any
  language with stdin/stdout works.
