//! Command-line definition, parsed by `clap`. Enum-like options stay `String` here and are
//! validated by the command handlers, so their error messages and exit codes (2 = usage) are
//! unchanged from the hand-written parser.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "protocol",
    version,
    about = "DeFi strategy execution protocol (simulation-only release)",
    after_help = "All strategy execution is simulated. Mainnet is not available in this release; `protocol aave account` can read (never write) a Sepolia testnet account.\n\
Amounts are decimal token amounts (e.g. 100.5). State lives in --home, $PROTOCOL_HOME or ~/.protocol."
)]
pub struct Cli {
    /// State directory (default: $PROTOCOL_HOME or ~/.protocol)
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// require (default): run external strategies in an OS sandbox (no network, no access to your
    /// files). off: run them with your user's full access.
    #[arg(long, global = true, value_name = "MODE", default_value = "require")]
    pub sandbox: String,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List or describe official and local strategies
    #[command(subcommand)]
    Strategies(StrategiesCmd),
    /// Scaffold a new strategy
    Create {
        /// Strategy name (lowercase letters, digits, '-' or '_')
        name: String,
        /// rust, c, cpp or wasm
        #[arg(long)]
        lang: Option<String>,
        /// Parent directory (default: ./custom-strategies)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Compile or check a strategy
    Build {
        /// Strategy name or directory
        strategy: String,
    },
    /// Run a strategy in the calm and stress scenarios
    Test {
        strategy: String,
        #[arg(long, default_value_t = 12)]
        ticks: u64,
    },
    /// Simulate a strategy without saving anything
    Simulate {
        strategy: String,
        #[arg(long, default_value_t = 10)]
        ticks: u64,
        /// calm or stress
        #[arg(long)]
        scenario: Option<String>,
        /// autonomous, approval or manual (default: autonomous)
        #[arg(long)]
        mode: Option<String>,
    },
    /// Run a strategy on LIVE read-only Aave testnet data (nothing executed, nothing saved)
    #[command(
        after_help = "Reads the account's Aave V3 totals from a Sepolia node every tick and feeds them to the strategy as an aggregate USD view (one pseudo-asset `USD`; no per-token rates). Choose the protocol with --protocol (aave, compound or morpho; default aave). Compound and Morpho read their own market (see `protocol compound account --help` and `protocol morpho account --help` for the environment variables) and support --on interval and --on block only; Morpho amounts are loan-token units, not dollars.\n\nRuns in manual mode: fund-moving proposals are only suggested, never executed. Needs PROTOCOL_RPC_URL and PROTOCOL_AAVE_POOL (see `protocol aave account --help`).\n\nWhat starts a tick (--on): `interval` (default: every --interval-ms), `block` (each new block, polled every --interval-ms) or `event` (only when the account has a Pool event: supply, withdraw, borrow, repay or liquidation; the first tick runs at once). Events are read with eth_getLogs over plain HTTP; a chain reorganisation can show an event that later disappears, and a gap of more than 500 blocks between polls is not backfilled. With block or event, waiting stops after --timeout-secs without a trigger."
    )]
    Watch {
        strategy: String,
        /// 0x address of the account to watch
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 5)]
        ticks: u64,
        /// Milliseconds between reads or polls (minimum 1000, be gentle with RPC providers)
        #[arg(long, default_value_t = 5000)]
        interval_ms: u64,
        /// What starts a tick: interval, block or event
        #[arg(long, default_value = "interval")]
        on: String,
        /// With --on block or event: stop after this many seconds without a trigger
        #[arg(long, default_value_t = 900)]
        timeout_secs: u64,
        /// Protocol to read: aave (default), compound or morpho
        #[arg(long, default_value = "aave")]
        protocol: String,
    },
    /// Serve a read-only status page on this device (127.0.0.1 only, secret link)
    #[command(
        after_help = "Starts a small web server bound to 127.0.0.1 (never a public interface) and prints a link with a random secret token. Open the link in a browser on this device. The page shows instances, status, kill switch, account, pending proposals, strategy state and the recent log. It is strictly read-only: it cannot approve, reject, run, stop or change anything; use the CLI for that. The token changes every run. Stop with Ctrl-C."
    )]
    Dashboard {
        /// Port to listen on (0 picks a free one)
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// Replay a strategy over recorded account history from a CSV file (nothing executed)
    #[command(
        after_help = "Feeds the strategy one recorded row per tick, exactly as `watch` would show it live (aggregate USD view), runs the risk engine in manual mode and reports what the strategy WOULD have proposed. Nothing is executed and no position is simulated: this answers 'when would it have warned me?', not 'how much would I have made?'.\n\nCSV (first line exactly): label,collateral_usd,debt_usd,liq_threshold_bps\nExample row: 2026-01-01,3000,2000,8500\nLines starting with # and blank lines are ignored. At most 100000 rows and 8 MiB. The report also states, from the data alone, the lowest health factor and how many ticks were below 1.0.\n\nThe history is whatever you supply: this command checks its format, not where it came from."
    )]
    Backtest {
        strategy: String,
        /// CSV file with the recorded history
        #[arg(long)]
        data: String,
        /// Protocol id the history stands for (aave, compound or morpho); must match the strategy's manifest
        #[arg(long, default_value = "aave")]
        protocol: String,
    },
    /// Show a strategy's content hash; --pin records it as reviewed
    Verify {
        /// Strategy directory
        dir: String,
        #[arg(long)]
        pin: bool,
    },
    /// Run a strategy continuously (ticks = 0: until stopped)
    Run {
        strategy: String,
        /// approval (default), autonomous or manual
        #[arg(long)]
        mode: Option<String>,
        /// simulation (default); testnet and mainnet are refused in this release
        #[arg(long)]
        network: Option<String>,
        /// calm or stress
        #[arg(long)]
        scenario: Option<String>,
        #[arg(long, default_value_t = 10)]
        ticks: u64,
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
        #[arg(long, default_value_t = 3)]
        max_failures: u32,
        /// Instance name (default: the strategy name)
        #[arg(long)]
        instance: Option<String>,
    },
    /// List instances
    Ps,
    /// Show state, account, pending actions and recent audit log
    Inspect { instance: String },
    /// Pause a running instance
    Pause { instance: String },
    /// Resume a paused instance
    Resume { instance: String },
    /// Stop an instance
    Stop { instance: String },
    /// List actions waiting for approval
    Pending { instance: String },
    /// Approve a queued action
    Approve { instance: String, id: u64 },
    /// Reject a queued action
    Reject { instance: String, id: u64 },
    /// Request a manual action (risk-checked like any other)
    Act {
        instance: String,
        /// supply, withdraw or repay
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        asset: Option<String>,
        /// Decimal amount, e.g. 100.5
        #[arg(long)]
        amount: Option<String>,
        #[arg(long)]
        protocol: Option<String>,
    },
    /// Emergency stop for all fund-moving actions: on, off or status
    KillSwitch {
        /// on | off | status
        which: String,
    },
    /// Read-only Aave testnet data (needs PROTOCOL_RPC_URL and PROTOCOL_AAVE_POOL)
    #[command(subcommand)]
    Aave(AaveCmd),
    /// Read-only Compound III testnet data (needs PROTOCOL_RPC_URL and PROTOCOL_COMPOUND_COMET)
    #[command(subcommand)]
    Compound(CompoundCmd),
    /// Read-only Morpho Blue testnet data (needs PROTOCOL_RPC_URL, PROTOCOL_MORPHO_ADDRESS, PROTOCOL_MORPHO_MARKET_ID)
    #[command(subcommand)]
    Morpho(MorphoCmd),
    /// Print the version
    Version,
}

#[derive(Subcommand, Debug)]
pub enum MorphoCmd {
    /// Show an account's collateral, debt and health factor in one Morpho Blue market (read-only; Sepolia testnet only)
    #[command(
        after_help = "Reads the market's parameters, the account's position and the oracle price with eth_call, then applies Morpho Blue's own health rule. Nothing is signed or sent.\n\
Environment (never flags or files, the RPC URL usually contains an API key):\n\
  PROTOCOL_RPC_URL           https JSON-RPC URL of a Sepolia node\n\
  PROTOCOL_MORPHO_ADDRESS    Morpho Blue contract address from Morpho's own deployment list\n\
  PROTOCOL_MORPHO_MARKET_ID  32-byte market id (0x + 64 hex digits)\n\
Other chains (including mainnet) are refused. Amounts are in the loan token (not dollars: Morpho has no USD price). Morpho's views do not accrue interest, so the debt shown is as of the market's last update and the health factor can be slightly optimistic."
    )]
    Account {
        /// 0x address to inspect
        address: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CompoundCmd {
    /// Show an account's balances, collateral, debt and health factor in one Comet market (read-only; Sepolia testnet only)
    #[command(
        after_help = "Reads the market's configuration and the account's balances and prices with eth_call, then applies Compound III's own liquidation rule. It also asks the contract's `isLiquidatable` and warns if the two disagree. Nothing is signed or sent.\n\
Environment (never flags or files, the RPC URL usually contains an API key):\n\
  PROTOCOL_RPC_URL        https JSON-RPC URL of a Sepolia node\n\
  PROTOCOL_COMPOUND_COMET Comet (market) address from Compound's own deployment list\n\
Other chains (including mainnet) are refused. The health factor is slightly conservative: borrowBalanceOf includes interest up to now, while the contract's check uses the last stored index."
    )]
    Account {
        /// 0x address to inspect
        address: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum AaveCmd {
    /// Show an account's collateral, debt and health factor (read-only; Sepolia testnet only)
    #[command(
        after_help = "Reads Aave V3 `Pool.getUserAccountData` with a single eth_call. Nothing is signed or sent.\n\
Environment (never flags or files, the RPC URL usually contains an API key):\n\
  PROTOCOL_RPC_URL    https JSON-RPC URL of a Sepolia node\n\
  PROTOCOL_AAVE_POOL  Aave V3 Sepolia Pool address from Aave's official address book\n\
Other chains (including mainnet) are refused."
    )]
    Account {
        /// 0x address to inspect
        address: String,
    },
    /// Simulate a supply, withdraw, borrow or repay with eth_call (nothing is signed or sent)
    #[command(
        after_help = "Builds the exact calldata a real Pool action would carry and asks the node to execute it
without broadcasting, as if the account had sent it, against the latest block. The answer is
WOULD SUCCEED or WOULD REVERT with the reason. Nothing is signed or sent; no key is involved.
It does not say the action is a good idea, nor that state will not change before a real
transaction. A supply for an account that lacks the tokens or the allowance to the Pool reverts:
that is the real answer.
Environment (never flags or files): PROTOCOL_RPC_URL, PROTOCOL_AAVE_POOL (see `protocol aave account --help`).
The asset is the token's 0x address from Aave's official address book for the same testnet.
Example: protocol aave preflight supply --asset 0x... --amount 10 --account 0x..."
    )]
    Preflight {
        /// supply, withdraw, borrow or repay
        action: String,
        /// 0x address of the token (from Aave's official address book)
        #[arg(long)]
        asset: String,
        /// Amount in whole tokens, for example 10 or 0.5
        #[arg(long)]
        amount: String,
        /// 0x address that would send the transaction and receive the result
        #[arg(long)]
        account: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum StrategiesCmd {
    /// List official and local strategies
    List,
    /// Permissions, parameters, trust tier and docs
    Info {
        /// Strategy name or directory
        strategy: String,
    },
}
