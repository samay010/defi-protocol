//! Read-only HTTPS JSON-RPC transport for the Aave adapter.
//!
//! What this crate can do: open an HTTP provider, check the chain id against the testnet
//! allowlist, and run `eth_call` at the latest block (optionally as if sent by an account, to
//! preflight an action; the call is executed by the node and never broadcast). What it cannot do: sign, send
//! transactions or hold keys. It exposes nothing but [`EthCall`], and alloy is built here with
//! default features off (no signer). No error from this crate contains the RPC URL (it usually
//! carries an API key).
//!
//! This is the only crate in the workspace that touches the network. Strategies never do: they
//! run in the offline sandbox (ADR 0012).

use std::fmt::Display;
use std::time::Duration;

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address as RawAddress, Bytes};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use protocol_adapter_aave::onchain::{
    bytes_to_hex, check_rpc_url, ensure_allowed_chain, hex_to_bytes, redact_url, Address,
    ChainError, EthCall,
};

/// Environment variable holding the RPC URL (never a CLI flag or a file in the repo).
pub const ENV_RPC_URL: &str = "PROTOCOL_RPC_URL";
/// Environment variable holding the Aave V3 Pool address, copied from Aave's official address
/// book for the chosen testnet. There is deliberately no built-in default.
pub const ENV_POOL: &str = "PROTOCOL_AAVE_POOL";

const TIMEOUT: Duration = Duration::from_secs(15);

struct Http<P> {
    // field order matters: the provider is dropped before the runtime it was used on
    provider: P,
    rt: tokio::runtime::Runtime,
    url: String,
}

impl<P> Http<P> {
    fn scrub<E: Display>(&self, e: E) -> ChainError {
        ChainError::Transport(redact_url(&e.to_string(), &self.url))
    }
}

impl<P: Provider> Http<P> {
    fn call_tx(&self, tx: TransactionRequest) -> Result<String, ChainError> {
        // the timeout must be created inside the runtime (its timer needs the runtime context)
        let fut = async { tokio::time::timeout(TIMEOUT, self.provider.call(tx)).await };
        let out: Bytes = self
            .rt
            .block_on(fut)
            .map_err(|_| ChainError::Transport("rpc request timed out".into()))?
            .map_err(|e| self.scrub(e))?;
        Ok(bytes_to_hex(&out))
    }
}

impl<P: Provider> EthCall for Http<P> {
    fn eth_call(&self, to: &Address, data_hex: &str) -> Result<String, ChainError> {
        let data = hex_to_bytes(data_hex)?;
        let tx = TransactionRequest::default()
            .with_to(RawAddress::from(to.0))
            .with_input(Bytes::from(data));
        self.call_tx(tx)
    }

    fn eth_call_from(
        &self,
        from: &Address,
        to: &Address,
        data_hex: &str,
    ) -> Result<String, ChainError> {
        let data = hex_to_bytes(data_hex)?;
        let tx = TransactionRequest::default()
            .with_from(RawAddress::from(from.0))
            .with_to(RawAddress::from(to.0))
            .with_input(Bytes::from(data));
        self.call_tx(tx)
    }
}

/// Connect to `rpc_url`, verify it serves an allowed (test) chain, and return a read-only
/// `eth_call` transport. Fails closed: bad URL, unreachable node or wrong chain is an error.
pub fn connect(rpc_url: &str) -> Result<impl EthCall, ChainError> {
    check_rpc_url(rpc_url)?;
    let parsed = rpc_url
        .parse()
        .map_err(|_| ChainError::Transport("invalid RPC URL".into()))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ChainError::Transport(format!("cannot start runtime: {e}")))?;
    let http = Http {
        provider: ProviderBuilder::new().connect_http(parsed),
        rt,
        url: rpc_url.to_string(),
    };
    let fut = async { tokio::time::timeout(TIMEOUT, http.provider.get_chain_id()).await };
    let chain_id = http
        .rt
        .block_on(fut)
        .map_err(|_| ChainError::Transport("rpc request timed out".into()))?
        .map_err(|e| http.scrub(e))?;
    ensure_allowed_chain(chain_id)?;
    Ok(http)
}
