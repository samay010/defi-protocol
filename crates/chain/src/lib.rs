//! Shared read-only chain primitives: addresses, 256-bit values, hex helpers, the testnet
//! allowlist, RPC URL checks, and the [`EthCall`] transport trait.
//!
//! Pure logic only: no I/O, no keys, nothing that can sign or send. A transport (HTTP JSON-RPC,
//! in `protocol-rpc`) is plugged in behind [`EthCall`], so every protocol reader built on top of
//! this crate (Aave, Compound, ...) is testable offline.

use std::fmt;

/// A 20-byte account/contract address. No EIP-55 checksum check (that needs Keccak, which we
/// do not ship); validity here means "0x + 40 hex digits" and nothing more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Address(pub [u8; 20]);

impl Address {
    pub fn parse(s: &str) -> Result<Address, ChainError> {
        let hex = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .ok_or_else(|| ChainError::BadAddress("must start with 0x".into()))?;
        if hex.len() != 40 {
            return Err(ChainError::BadAddress(format!(
                "expected 40 hex digits, got {}",
                hex.len()
            )));
        }
        let mut out = [0u8; 20];
        let bytes = decode_hex(hex).map_err(|e| ChainError::BadAddress(e.to_string()))?;
        out.copy_from_slice(&bytes);
        if out == [0u8; 20] {
            return Err(ChainError::BadAddress("zero address".into()));
        }
        Ok(Address(out))
    }

    pub fn to_hex(&self) -> String {
        encode_hex_prefixed(&self.0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChainError {
    BadAddress(String),
    BadHex(String),
    /// A reply did not have the expected length in bytes.
    BadLength(usize),
    /// A value does not fit the type we need (never silently truncated).
    Overflow(&'static str),
    Transport(String),
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainError::BadAddress(m) => write!(f, "bad address: {m}"),
            ChainError::BadHex(m) => write!(f, "bad hex: {m}"),
            ChainError::BadLength(n) => {
                write!(f, "unexpected reply length {n} bytes")
            }
            ChainError::Overflow(w) => write!(f, "value too large for {w}"),
            ChainError::Transport(m) => write!(f, "rpc: {m}"),
        }
    }
}

impl std::error::Error for ChainError {}

/// A 256-bit unsigned integer as the 32 big-endian bytes the EVM uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct U256(pub [u8; 32]);

impl U256 {
    pub const MAX: U256 = U256([0xff; 32]);

    pub fn to_u128(&self) -> Option<u128> {
        if self.0[..16].iter().any(|b| *b != 0) {
            return None;
        }
        let mut b = [0u8; 16];
        b.copy_from_slice(&self.0[16..]);
        Some(u128::from_be_bytes(b))
    }

    /// Lossy conversion for display and comparisons only (never for amounts).
    pub fn to_f64_lossy(&self) -> f64 {
        self.0
            .iter()
            .fold(0.0, |acc, b| acc * 256.0 + f64::from(*b))
    }
}

/// The one capability a transport must offer: a read-only `eth_call` at the latest block.
/// There is intentionally no method for sending transactions or signing.
pub trait EthCall {
    fn eth_call(&self, to: &Address, data_hex: &str) -> Result<String, ChainError>;

    /// The same read-only call, executed as if `from` had sent it (used to preflight an action
    /// against the account's real balances and allowances). Nothing is signed or sent. A
    /// transport that cannot do this keeps the default, which fails with a clear message.
    fn eth_call_from(
        &self,
        _from: &Address,
        _to: &Address,
        _data_hex: &str,
    ) -> Result<String, ChainError> {
        Err(ChainError::Transport(
            "this transport cannot simulate calls from an account".into(),
        ))
    }

    /// The latest block number (read-only).
    fn block_number(&self) -> Result<u64, ChainError> {
        Err(ChainError::Transport(
            "this transport cannot read blocks or logs".into(),
        ))
    }

    /// `eth_getLogs` over the inclusive block range `from..=to` (read-only).
    fn get_logs(&self, _q: &LogQuery, _from: u64, _to: u64) -> Result<Vec<RawLog>, ChainError> {
        Err(ChainError::Transport(
            "this transport cannot read blocks or logs".into(),
        ))
    }
}

/// An `eth_getLogs` filter: a contract and, per topic position, an optional list of accepted
/// values (`0x` + 64 hex digits each). `None` means "any".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogQuery {
    pub address: Address,
    pub topics: [Option<Vec<String>>; 4],
}

/// One log as returned by the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLog {
    pub block_number: u64,
    pub log_index: Option<u64>,
    pub tx_hash: Option<String>,
    pub topics: Vec<String>,
    /// `0x` hex of the non-indexed data.
    pub data: String,
}

/// Chain ids this release may talk to. Testnets only until audits (ADR 0006).
/// 11155111 = Ethereum Sepolia.
pub const ALLOWED_CHAIN_IDS: [u64; 1] = [11_155_111];

/// Refuse any chain that is not on the testnet allowlist (mainnet included).
pub fn ensure_allowed_chain(chain_id: u64) -> Result<(), ChainError> {
    if ALLOWED_CHAIN_IDS.contains(&chain_id) {
        Ok(())
    } else {
        Err(ChainError::Transport(format!(
            "chain id {chain_id} is not allowed (testnets only: {ALLOWED_CHAIN_IDS:?})"
        )))
    }
}

/// An RPC URL must be https, or plain http only to this machine. URLs usually carry an API
/// key, so no error produced from here includes the URL itself.
pub fn check_rpc_url(url: &str) -> Result<(), ChainError> {
    let bad = |why: &str| Err(ChainError::Transport(format!("invalid RPC URL: {why}")));
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("contains whitespace or control characters");
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return if rest.is_empty() || rest.starts_with('/') {
            bad("missing host")
        } else {
            Ok(())
        };
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = if rest.starts_with('[') {
            // IPv6 literal: keep everything up to and including the closing bracket
            rest.find(']').map_or("", |i| &rest[..=i])
        } else {
            rest.split(['/', ':', '?', '#']).next().unwrap_or("")
        };
        return if ["localhost", "127.0.0.1", "[::1]"].contains(&host) {
            Ok(())
        } else {
            bad("plain http is only allowed for localhost")
        };
    }
    bad("must start with https://")
}

/// Replace every occurrence of the RPC URL (and its path/query, where keys live) in `msg`.
pub fn redact_url(msg: &str, url: &str) -> String {
    let mut out = if url.is_empty() {
        msg.to_string()
    } else {
        msg.replace(url, "<rpc-url>")
    };
    // also the part after the host, in case a library prints only the path
    if let Some(i) = url.find("://") {
        let after = &url[i + 3..];
        if let Some(slash) = after.find('/') {
            let path = &after[slash..];
            if path.len() > 1 {
                out = out.replace(path, "/<redacted>");
            }
        }
    }
    out
}

/// `0x`-prefixed hex to bytes.
pub fn hex_to_bytes(s: &str) -> Result<Vec<u8>, ChainError> {
    let hex = s
        .strip_prefix("0x")
        .ok_or_else(|| ChainError::BadHex("missing 0x".into()))?;
    decode_hex(hex).map_err(ChainError::BadHex)
}

/// Bytes to `0x`-prefixed lowercase hex.
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    encode_hex_prefixed(bytes)
}

pub fn encode_hex_prefixed(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() & 1 == 1 {
        return Err("odd number of hex digits".into());
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("non-hex character {:?}", c as char)),
    };
    s.as_bytes()
        .chunks(2)
        .map(|p| Ok(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn address_parsing_is_strict() {
        assert!(Address::parse(USER).is_ok());
        for bad in [
            "",
            "1111111111111111111111111111111111111111",
            "0x11",
            "0x111111111111111111111111111111111111111g",
            "0x11111111111111111111111111111111111111111",
            "0x0000000000000000000000000000000000000000",
        ] {
            assert!(Address::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Address::parse(USER).unwrap().to_hex(), USER);
        // mixed case is accepted (checksum is not verified, see docs) and normalised to lower
        let a = Address::parse("0xAbCdEf0000000000000000000000000000000001").unwrap();
        assert_eq!(a.to_hex(), "0xabcdef0000000000000000000000000000000001");
    }

    #[test]
    fn only_testnets_are_allowed() {
        assert!(ensure_allowed_chain(11_155_111).is_ok());
        for id in [1u64, 10, 137, 8453, 42161, 0] {
            assert!(ensure_allowed_chain(id).is_err(), "{id}");
        }
    }

    #[test]
    fn rpc_url_must_be_https_or_local_http() {
        for ok in [
            "https://eth-sepolia.example.com/v2/KEY",
            "https://rpc.example.org",
            "http://localhost:8545",
            "http://127.0.0.1:8545/",
            "http://[::1]:8545",
        ] {
            assert!(check_rpc_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "https://",
            "https:///x",
            "http://example.com/v2/KEY",
            "http://localhost.evil.com/x",
            "ftp://x.example.com",
            "ws://localhost:8546",
            "example.com",
            "https://a.example.com/ x",
            "https://a.example.com/\n",
        ] {
            assert!(check_rpc_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn errors_never_contain_the_rpc_url_or_its_key() {
        let url = "https://eth-sepolia.example.com/v2/SUPERSECRETKEY";
        let msg =
            format!("error sending request for url ({url}): timed out; path /v2/SUPERSECRETKEY");
        let out = redact_url(&msg, url);
        assert!(!out.contains("SUPERSECRETKEY"), "{out}");
        assert!(out.contains("<rpc-url>"));
        // a bad-URL error must not echo the URL either
        let e = check_rpc_url("http://example.com/v2/SUPERSECRETKEY").unwrap_err();
        assert!(!e.to_string().contains("SUPERSECRETKEY"));
    }

    #[test]
    fn hex_helpers_round_trip() {
        let b = hex_to_bytes("0x00ff10").unwrap();
        assert_eq!(b, vec![0, 255, 16]);
        assert_eq!(bytes_to_hex(&b), "0x00ff10");
        assert!(hex_to_bytes("00ff").is_err());
        assert!(hex_to_bytes("0xf").is_err());
    }
}
