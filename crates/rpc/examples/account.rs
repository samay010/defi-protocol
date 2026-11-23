//! Manual testnet check: print an account's Aave V3 data (read-only).
//!
//!   export PROTOCOL_RPC_URL='https://...sepolia...'      # keep it out of shell history/files
//!   export PROTOCOL_AAVE_POOL='0x...'                     # from Aave's official address book
//!   cargo run -p protocol-rpc --example account -- 0xYourTestnetAddress
//!
//! Compare the numbers with the Aave UI for the same address before trusting any scale.

use protocol_adapter_aave::onchain::{AaveReader, Address};
use protocol_rpc::{connect, ENV_POOL, ENV_RPC_URL};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let user_arg = std::env::args()
        .nth(1)
        .ok_or("usage: account <0x-address-to-inspect>")?;
    let url = std::env::var(ENV_RPC_URL).map_err(|_| format!("{ENV_RPC_URL} is not set"))?;
    let pool = std::env::var(ENV_POOL).map_err(|_| format!("{ENV_POOL} is not set"))?;

    let user = Address::parse(&user_arg)?;
    let pool = Address::parse(&pool)?;
    let reader = AaveReader::new(connect(&url)?, pool);
    let d = reader.account_data(&user)?;

    println!("(read-only; raw on-chain integers, scales unconfirmed, see ADR 0013)");
    println!(
        "total_collateral_base       = {:?}",
        d.total_collateral_base.to_u128()
    );
    println!(
        "total_debt_base             = {:?}",
        d.total_debt_base.to_u128()
    );
    println!(
        "available_borrows_base      = {:?}",
        d.available_borrows_base.to_u128()
    );
    println!(
        "current_liquidation_threshold = {:?}",
        d.current_liquidation_threshold.to_u128()
    );
    println!("ltv                         = {:?}", d.ltv.to_u128());
    match d.health_factor_f64() {
        Some(hf) => println!("health_factor               = {hf} (assuming 1e18 scale)"),
        None => println!("health_factor               = no debt"),
    }
    Ok(())
}
