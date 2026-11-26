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

pub use protocol_chain::*;

/// `bytes4(keccak256("getUserAccountData(address)"))`, verified independently.
pub const GET_USER_ACCOUNT_DATA_SELECTOR: [u8; 4] = [0xbf, 0x92, 0x85, 0x7c];

/// Fixed-point scale of `healthFactor` (1e18). Confirmed from Aave's `GenericLogic`.
pub const HEALTH_FACTOR_SCALE: f64 = 1e18;

/// One USD in base-currency units (8 decimals). Confirmed on-chain via the Sepolia
/// oracle's `BASE_CURRENCY_UNIT()`; the base currency is USD.
pub const BASE_CURRENCY_UNIT: f64 = 1e8;

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

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "0x1111111111111111111111111111111111111111";
    const POOL: &str = "0x2222222222222222222222222222222222222222";

    fn word(n: u128) -> String {
        format!("{:064x}", n)
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
