//! Read-only Aave V3 account lookup: ABI encoding/decoding only.
//!
//! This module is pure logic. It does no I/O, holds no keys and cannot sign or send
//! anything. The only call it knows how to build is `getUserAccountData(address)` on the Aave
//! V3 `Pool` (a `view` function), encoded for an `eth_call`. A transport (HTTP JSON-RPC) is
//! plugged in behind the [`EthCall`] trait, so everything here is testable offline.
//!
//! Sources: signature and the order of the six return values are from the official Aave docs
//! ("Pool" page, `getUserAccountData`). The 4-byte selector was recomputed with an
//! independent Keccak-256 implementation. Pool ADDRESSES are deliberately not baked in: the
//! caller must supply one copied from Aave's official address book for the chosen testnet.
//!
//! Scales, confirmed on 2026-11-24 from Aave's own sources (not from memory):
//! - `healthFactor` has 18 decimals and is `type(uint256).max` when the account has no debt
//!   (comment and code in `GenericLogic.calculateUserAccountData`, aave-v3-origin).
//! - Base-currency amounts are USD with 8 decimals: the Sepolia `AaveOracle` returned
//!   `BASE_CURRENCY_UNIT() = 100000000` and `BASE_CURRENCY()` = the zero address (USD).
//!
//! Still to be matched against the Aave UI with a real position: `ltv` and
//! `currentLiquidationThreshold` are treated as basis points (the contract divides by 100_00).

use std::fmt;

/// `bytes4(keccak256("getUserAccountData(address)"))`, verified independently.
pub const GET_USER_ACCOUNT_DATA_SELECTOR: [u8; 4] = [0xbf, 0x92, 0x85, 0x7c];

/// Fixed-point scale of `healthFactor` (1e18). Confirmed from Aave's `GenericLogic`.
pub const HEALTH_FACTOR_SCALE: f64 = 1e18;

/// One USD in base-currency units (8 decimals). Confirmed on-chain via the Sepolia
/// oracle's `BASE_CURRENCY_UNIT()`; the base currency is USD.
pub const BASE_CURRENCY_UNIT: f64 = 1e8;

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
    /// The reply was not exactly six 32-byte words.
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
                write!(f, "unexpected reply length {n} bytes (want 192)")
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

/// Result of `getUserAccountData`, raw on-chain integers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AccountData {
    pub total_collateral_base: U256,
    pub total_debt_base: U256,
    pub available_borrows_base: U256,
    pub current_liquidation_threshold: U256,
    pub ltv: U256,
    pub health_factor: U256,
}

impl AccountData {
    /// True when the account has no debt. Aave reports `type(uint256).max` as the health
    /// factor in that case; treat it as "no risk", not as a huge number.
    pub fn has_no_debt(&self) -> bool {
        self.health_factor == U256::MAX
    }

    /// Health factor as a float, `None` when there is no debt. Display/threshold use only.
    pub fn health_factor_f64(&self) -> Option<f64> {
        if self.has_no_debt() {
            None
        } else {
            Some(self.health_factor.to_f64_lossy() / HEALTH_FACTOR_SCALE)
        }
    }

    /// Total collateral in USD (display/threshold use only; lossy float).
    pub fn total_collateral_usd(&self) -> f64 {
        self.total_collateral_base.to_f64_lossy() / BASE_CURRENCY_UNIT
    }

    /// Total debt in USD (display/threshold use only; lossy float).
    pub fn total_debt_usd(&self) -> f64 {
        self.total_debt_base.to_f64_lossy() / BASE_CURRENCY_UNIT
    }

    /// Remaining borrowing power in USD (display only).
    pub fn available_borrows_usd(&self) -> f64 {
        self.available_borrows_base.to_f64_lossy() / BASE_CURRENCY_UNIT
    }

    /// `ltv` as a percentage (basis points / 100). Basis points are assumed, see module docs.
    pub fn ltv_percent(&self) -> f64 {
        self.ltv.to_f64_lossy() / 100.0
    }

    /// `currentLiquidationThreshold` as a percentage (basis points / 100).
    pub fn liquidation_threshold_percent(&self) -> f64 {
        self.current_liquidation_threshold.to_f64_lossy() / 100.0
    }

    pub fn total_debt_base_u128(&self) -> Result<u128, ChainError> {
        self.total_debt_base
            .to_u128()
            .ok_or(ChainError::Overflow("total_debt_base"))
    }
}

/// Calldata (`0x`-prefixed hex) for `Pool.getUserAccountData(user)`.
pub fn encode_get_user_account_data(user: &Address) -> String {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&GET_USER_ACCOUNT_DATA_SELECTOR);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(&user.0);
    encode_hex_prefixed(&data)
}

/// Decode the `eth_call` result (`0x`-prefixed hex) into [`AccountData`].
pub fn decode_account_data(result_hex: &str) -> Result<AccountData, ChainError> {
    let hex = result_hex
        .strip_prefix("0x")
        .ok_or_else(|| ChainError::BadHex("missing 0x".into()))?;
    let bytes = decode_hex(hex).map_err(ChainError::BadHex)?;
    if bytes.len() != 192 {
        return Err(ChainError::BadLength(bytes.len()));
    }
    let word = |i: usize| {
        let mut w = [0u8; 32];
        w.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
        U256(w)
    };
    Ok(AccountData {
        total_collateral_base: word(0),
        total_debt_base: word(1),
        available_borrows_base: word(2),
        current_liquidation_threshold: word(3),
        ltv: word(4),
        health_factor: word(5),
    })
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

/// Read-only view of one Aave V3 Pool.
pub struct AaveReader<T: EthCall> {
    transport: T,
    pool: Address,
}

impl<T: EthCall> AaveReader<T> {
    pub fn new(transport: T, pool: Address) -> AaveReader<T> {
        AaveReader { transport, pool }
    }

    pub fn pool(&self) -> &Address {
        &self.pool
    }

    /// A read-only `eth_call` to any contract, optionally as if sent by `from`.
    pub fn call_from(
        &self,
        from: Option<&Address>,
        to: &Address,
        data_hex: &str,
    ) -> Result<String, ChainError> {
        match from {
            Some(f) => self.transport.eth_call_from(f, to, data_hex),
            None => self.transport.eth_call(to, data_hex),
        }
    }

    /// The latest block number.
    pub fn block_number(&self) -> Result<u64, ChainError> {
        self.transport.block_number()
    }

    /// Logs matching `q` over the inclusive range `from..=to`.
    pub fn logs(&self, q: &LogQuery, from: u64, to: u64) -> Result<Vec<RawLog>, ChainError> {
        self.transport.get_logs(q, from, to)
    }

    #[cfg(test)]
    pub(crate) fn transport_for_tests(&self) -> &T {
        &self.transport
    }

    pub fn account_data(&self, user: &Address) -> Result<AccountData, ChainError> {
        let reply = self
            .transport
            .eth_call(&self.pool, &encode_get_user_account_data(user))?;
        decode_account_data(&reply)
    }
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

fn encode_hex_prefixed(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
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
    const POOL: &str = "0x2222222222222222222222222222222222222222";

    fn word(n: u128) -> String {
        format!("{:064x}", n)
    }

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
    fn calldata_is_selector_plus_padded_address() {
        let u = Address::parse(USER).unwrap();
        assert_eq!(
            encode_get_user_account_data(&u),
            format!("0xbf92857c{}{}", "0".repeat(24), "1".repeat(40))
        );
    }

    #[test]
    fn decodes_six_words_in_documented_order() {
        let reply = format!(
            "0x{}{}{}{}{}{}",
            word(1000),
            word(400),
            word(300),
            word(8250),
            word(8000),
            word(2_500_000_000_000_000_000)
        );
        let d = decode_account_data(&reply).unwrap();
        assert_eq!(d.total_collateral_base.to_u128(), Some(1000));
        assert_eq!(d.total_debt_base.to_u128(), Some(400));
        assert_eq!(d.available_borrows_base.to_u128(), Some(300));
        assert_eq!(d.current_liquidation_threshold.to_u128(), Some(8250));
        assert_eq!(d.ltv.to_u128(), Some(8000));
        assert!(!d.has_no_debt());
        assert_eq!(d.health_factor_f64(), Some(2.5));
        assert_eq!(d.total_debt_base_u128(), Ok(400));
    }

    #[test]
    fn base_amounts_are_usd_with_eight_decimals() {
        // 1_500.25 USD collateral, 400 USD debt, in 1e8 base units
        let reply = format!(
            "0x{}{}{}{}{}{}",
            word(150_025_000_000),
            word(40_000_000_000),
            word(0),
            word(8250),
            word(8000),
            word(2_000_000_000_000_000_000)
        );
        let d = decode_account_data(&reply).unwrap();
        assert!((d.total_collateral_usd() - 1500.25).abs() < 1e-9);
        assert!((d.total_debt_usd() - 400.0).abs() < 1e-9);
        assert_eq!(d.health_factor_f64(), Some(2.0));
        assert_eq!(d.available_borrows_usd(), 0.0);
        assert!((d.ltv_percent() - 80.0).abs() < 1e-9);
        assert!((d.liquidation_threshold_percent() - 82.5).abs() < 1e-9);
    }

    #[test]
    fn max_health_factor_means_no_debt_not_a_huge_number() {
        let reply = format!(
            "0x{}{}{}{}{}{}",
            word(1000),
            word(0),
            word(800),
            word(8250),
            word(8000),
            "f".repeat(64)
        );
        let d = decode_account_data(&reply).unwrap();
        assert!(d.has_no_debt());
        assert_eq!(d.health_factor_f64(), None);
    }

    #[test]
    fn malformed_replies_are_errors_never_panics() {
        assert!(matches!(
            decode_account_data(""),
            Err(ChainError::BadHex(_))
        ));
        assert!(matches!(
            decode_account_data("0x"),
            Err(ChainError::BadLength(0))
        ));
        assert!(matches!(
            decode_account_data("0xzz"),
            Err(ChainError::BadHex(_))
        ));
        assert!(matches!(
            decode_account_data("0xabc"),
            Err(ChainError::BadHex(_))
        ));
        let short = format!("0x{}", word(1).repeat(5));
        assert_eq!(decode_account_data(&short), Err(ChainError::BadLength(160)));
        let long = format!("0x{}", word(1).repeat(7));
        assert_eq!(decode_account_data(&long), Err(ChainError::BadLength(224)));
    }

    #[test]
    fn oversized_values_are_reported_not_truncated() {
        let mut w = [0u8; 32];
        w[0] = 1;
        assert_eq!(U256(w).to_u128(), None);
        let d = AccountData {
            total_collateral_base: U256(w),
            total_debt_base: U256(w),
            available_borrows_base: U256(w),
            current_liquidation_threshold: U256(w),
            ltv: U256(w),
            health_factor: U256(w),
        };
        assert_eq!(
            d.total_debt_base_u128(),
            Err(ChainError::Overflow("total_debt_base"))
        );
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

    struct Fake {
        reply: Result<String, ChainError>,
    }
    impl EthCall for Fake {
        fn eth_call(&self, to: &Address, data_hex: &str) -> Result<String, ChainError> {
            assert_eq!(to.to_hex(), POOL);
            assert!(data_hex.starts_with("0xbf92857c"));
            match &self.reply {
                Ok(s) => Ok(s.clone()),
                Err(_) => Err(ChainError::Transport("down".into())),
            }
        }
    }

    #[test]
    fn reader_wires_encode_transport_decode_and_surfaces_transport_errors() {
        let reply = format!("0x{}", [1u128, 2, 3, 4, 5, 6].map(word).concat());
        let r = AaveReader::new(Fake { reply: Ok(reply) }, Address::parse(POOL).unwrap());
        let d = r.account_data(&Address::parse(USER).unwrap()).unwrap();
        assert_eq!(d.ltv.to_u128(), Some(5));

        let bad = AaveReader::new(
            Fake {
                reply: Err(ChainError::Transport(String::new())),
            },
            Address::parse(POOL).unwrap(),
        );
        assert_eq!(
            bad.account_data(&Address::parse(USER).unwrap()),
            Err(ChainError::Transport("down".into()))
        );
    }
}
