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
        /// rust, c, cpp, python or java
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
        after_help = "Reads the account's Aave V3 totals from a Sepolia node every tick and feeds them to the strategy as an aggregate USD view (one pseudo-asset `USD`; no per-token rates). Runs in manual mode: fund-moving proposals are only suggested, never executed. Needs PROTOCOL_RPC_URL and PROTOCOL_AAVE_POOL (see `protocol aave account --help`)."
    )]
    Watch {
        strategy: String,
        /// 0x address of the account to watch
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 5)]
        ticks: u64,
        /// Milliseconds between reads (minimum 1000, be gentle with RPC providers)
        #[arg(long, default_value_t = 5000)]
        interval_ms: u64,
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
    /// Print the version
    Version,
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
