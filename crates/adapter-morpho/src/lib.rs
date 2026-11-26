//! Read-only Morpho Blue account reader (one market, one account).
//!
//! Pure ABI logic on top of [`protocol_chain::EthCall`]: no keys, only `eth_call`s.
//!
//! What was checked against Morpho's own material (2026-11-26), and what was not:
//! - `Morpho.sol` (`morpho-org/morpho-blue`, main): `_isHealthy` is read verbatim:
//!   no borrow shares -> healthy; else `borrowed = borrowShares.toAssetsUp(totalBorrowAssets,
//!   totalBorrowShares)`, `maxBorrow = collateral.mulDivDown(price, ORACLE_PRICE_SCALE)
//!   .wMulDown(lltv)`, healthy iff `maxBorrow >= borrowed`. `liquidate` requires `!_isHealthy`.
//! - Morpho's docs (`docs.morpho.org/developers/contracts/morpho`): the exact signatures and
//!   field order of `position`, `market` and `idToMarketParams`; `market` returns values as of the
//!   last update (interest is NOT accrued by the view); `lltv` has 18 decimals.
//! - Morpho's own JS SDK (`@morpho-org/blue-sdk` 5.22.1, `SharesMath.d.ts`): `VIRTUAL_SHARES =
//!   1e6`, `VIRTUAL_ASSETS = 1`. (One third-party page claimed `VIRTUAL_ASSETS = 1e6`; the
//!   official SDK disagrees and matches the contract, so that page is wrong.)
//! - NOT read directly from source: `SharesMathLib.sol` itself and the `1e36` oracle scale
//!   (`ORACLE_PRICE_SCALE`) were not reachable from the sandbox; the formulas
//!   `toAssetsUp = mulDivUp(shares, assets + 1, totalShares + 1e6)` and the 1e36 scale follow
//!   Morpho's documented design and the SDK, and are listed in `docs/audit-scope.md` for a
//!   reviewer to confirm against the contract.
//!
//! Limits: because the views do not accrue interest, the debt read here is the debt at the last
//! market update. It can be lower than what a liquidator would face now, so this reader is
//! slightly OPTIMISTIC (the opposite of the Compound reader). Values are in raw loan-token units
//! (Morpho has no USD price); never treat them as dollars.
//!
//! Addresses and market ids are never built in: the caller supplies the Morpho address and the
//! 32-byte market id from Morpho's own deployment and market lists for the chosen testnet.

pub use protocol_chain::ChainError;
use protocol_chain::{bytes_to_hex, hex_to_bytes, Address, EthCall};
use protocol_core::readonly::{AccountSource, AggregateView};

pub const PROTOCOL_ID: &str = "morpho";

pub const POSITION_SELECTOR: [u8; 4] = [0x93, 0xc5, 0x20, 0x62];
pub const MARKET_SELECTOR: [u8; 4] = [0x5c, 0x60, 0xe3, 0x9a];
pub const ID_TO_MARKET_PARAMS_SELECTOR: [u8; 4] = [0x2c, 0x3c, 0x91, 0x57];
pub const ORACLE_PRICE_SELECTOR: [u8; 4] = [0xa0, 0x35, 0xb1, 0xfe];
pub const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

pub const VIRTUAL_SHARES: u128 = 1_000_000;
pub const VIRTUAL_ASSETS: u128 = 1;
/// `ORACLE_PRICE_SCALE`.
pub const ORACLE_PRICE_SCALE: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;
/// `WAD`.
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// A 32-byte market id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketId(pub [u8; 32]);

impl MarketId {
    /// `0x` + 64 hex digits.
    pub fn parse(s: &str) -> Result<MarketId, ChainError> {
        let b = hex_to_bytes(s.trim())?;
        let a: [u8; 32] = b.as_slice().try_into().map_err(|_| {
            ChainError::BadHex("a market id is 32 bytes (0x + 64 hex digits)".into())
        })?;
        if a == [0u8; 32] {
            return Err(ChainError::BadHex("zero market id".into()));
        }
        Ok(MarketId(a))
    }

    pub fn to_hex(&self) -> String {
        bytes_to_hex(&self.0)
    }
}

fn call_data(selector: [u8; 4], words: &[[u8; 32]]) -> String {
    let mut d = Vec::with_capacity(4 + words.len() * 32);
    d.extend_from_slice(&selector);
    for w in words {
        d.extend_from_slice(w);
    }
    bytes_to_hex(&d)
}

fn addr_word(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(&a.0);
    w
}

fn reply_words(reply: &str, n: usize) -> Result<Vec<[u8; 32]>, ChainError> {
    let b = hex_to_bytes(reply)?;
    if b.len() != n * 32 {
        return Err(ChainError::BadLength(b.len()));
    }
    Ok(b.chunks(32)
        .map(|c| {
            let mut w = [0u8; 32];
            w.copy_from_slice(c);
            w
        })
        .collect())
}

fn word_u128(w: &[u8; 32], what: &'static str) -> Result<u128, ChainError> {
    if w[..16].iter().any(|b| *b != 0) {
        return Err(ChainError::Overflow(what));
    }
    let mut b = [0u8; 16];
    b.copy_from_slice(&w[16..]);
    Ok(u128::from_be_bytes(b))
}

fn word_address(w: &[u8; 32], allow_zero: bool) -> Result<Address, ChainError> {
    if w[..12].iter().any(|b| *b != 0) {
        return Err(ChainError::BadAddress("not a padded address".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&w[12..]);
    if a == [0u8; 20] && !allow_zero {
        return Err(ChainError::BadAddress("zero address".into()));
    }
    Ok(Address(a))
}

/// Full-width `a * b / d` (256-bit intermediate), rounded down or up; an error when the result
/// does not fit in `u128` or `d == 0`.
pub fn mul_div(a: u128, b: u128, d: u128, round_up: bool) -> Result<u128, ChainError> {
    const WHAT: &str = "mulDiv";
    if d == 0 {
        return Err(ChainError::Overflow("division by zero"));
    }
    // 128 x 128 -> 256 bits as (hi, lo)
    let (a1, a0) = (a >> 64, a & u128::from(u64::MAX));
    let (b1, b0) = (b >> 64, b & u128::from(u64::MAX));
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 64) + (p01 & u128::from(u64::MAX)) + (p10 & u128::from(u64::MAX));
    let lo = (p00 & u128::from(u64::MAX)) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    // 256 / 128 long division, one bit at a time
    let (mut q_hi, mut q_lo, mut r) = (0u128, 0u128, 0u128);
    for i in (0..256).rev() {
        let bit = if i >= 128 {
            (hi >> (i - 128)) & 1
        } else {
            (lo >> i) & 1
        };
        let carry = r >> 127;
        r = (r << 1) | bit;
        if carry == 1 || r >= d {
            r = r.wrapping_sub(d);
            if i >= 128 {
                q_hi |= 1 << (i - 128);
            } else {
                q_lo |= 1 << i;
            }
        }
    }
    if q_hi != 0 {
        return Err(ChainError::Overflow(WHAT));
    }
    if round_up && r != 0 {
        return q_lo.checked_add(1).ok_or(ChainError::Overflow(WHAT));
    }
    Ok(q_lo)
}

/// `MarketParams` from `idToMarketParams`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketParams {
    pub loan_token: Address,
    pub collateral_token: Address,
    pub oracle: Address,
    /// May be the zero address (a market without an interest rate model).
    pub irm: Address,
    /// Liquidation LLTV, 18 decimals, at most 100%.
    pub lltv: u128,
}

pub fn decode_market_params(reply: &str) -> Result<MarketParams, ChainError> {
    let w = reply_words(reply, 5)?;
    let lltv = word_u128(&w[4], "lltv")?;
    if lltv == 0 || lltv > WAD {
        return Err(ChainError::Overflow("lltv out of range (0, 100%]"));
    }
    Ok(MarketParams {
        loan_token: word_address(&w[0], false).map_err(|_| no_market())?,
        collateral_token: word_address(&w[1], false)?,
        oracle: word_address(&w[2], false)?,
        irm: word_address(&w[3], true)?,
        lltv,
    })
}

fn no_market() -> ChainError {
    ChainError::BadAddress("this market id does not exist on this Morpho contract".into())
}

/// Market totals (`market(id)`), as of the last update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketTotals {
    pub total_supply_assets: u128,
    pub total_supply_shares: u128,
    pub total_borrow_assets: u128,
    pub total_borrow_shares: u128,
}

pub fn decode_market(reply: &str) -> Result<MarketTotals, ChainError> {
    let w = reply_words(reply, 6)?;
    Ok(MarketTotals {
        total_supply_assets: word_u128(&w[0], "totalSupplyAssets")?,
        total_supply_shares: word_u128(&w[1], "totalSupplyShares")?,
        total_borrow_assets: word_u128(&w[2], "totalBorrowAssets")?,
        total_borrow_shares: word_u128(&w[3], "totalBorrowShares")?,
    })
}

/// An account's stored position (`position(id, user)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawPosition {
    pub supply_shares: u128,
    pub borrow_shares: u128,
    pub collateral: u128,
}

pub fn decode_position(reply: &str) -> Result<RawPosition, ChainError> {
    let w = reply_words(reply, 3)?;
    Ok(RawPosition {
        supply_shares: word_u128(&w[0], "supplyShares")?,
        borrow_shares: word_u128(&w[1], "borrowShares")?,
        collateral: word_u128(&w[2], "collateral")?,
    })
}

/// An account in one market, with the derived numbers (loan-token raw units).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub raw: RawPosition,
    pub price: u128,
    pub lltv: u128,
    /// Supplied loan token (shares rounded down).
    pub supplied_assets: u128,
    /// Debt in loan token (shares rounded UP, as the contract does).
    pub borrowed_assets: u128,
    /// Collateral value in loan token.
    pub collateral_value: u128,
    /// `collateral_value * lltv`, rounded down: the most that may stay borrowed.
    pub max_borrow: u128,
}

impl Position {
    /// Derive the numbers exactly as `_isHealthy` does.
    pub fn new(
        params: &MarketParams,
        totals: &MarketTotals,
        raw: RawPosition,
        price: u128,
    ) -> Result<Position, ChainError> {
        let add = |a: u128, b: u128| a.checked_add(b).ok_or(ChainError::Overflow("total"));
        let supplied_assets = mul_div(
            raw.supply_shares,
            add(totals.total_supply_assets, VIRTUAL_ASSETS)?,
            add(totals.total_supply_shares, VIRTUAL_SHARES)?,
            false,
        )?;
        let borrowed_assets = mul_div(
            raw.borrow_shares,
            add(totals.total_borrow_assets, VIRTUAL_ASSETS)?,
            add(totals.total_borrow_shares, VIRTUAL_SHARES)?,
            true,
        )?;
        let collateral_value = mul_div(raw.collateral, price, ORACLE_PRICE_SCALE, false)?;
        let max_borrow = mul_div(collateral_value, params.lltv, WAD, false)?;
        Ok(Position {
            raw,
            price,
            lltv: params.lltv,
            supplied_assets,
            borrowed_assets,
            collateral_value,
            max_borrow,
        })
    }

    /// The contract's rule.
    pub fn healthy(&self) -> bool {
        self.raw.borrow_shares == 0 || self.max_borrow >= self.borrowed_assets
    }

    /// `max_borrow / debt`; `None` without debt. Below 1 means liquidatable.
    pub fn health_factor(&self) -> Option<f64> {
        (self.raw.borrow_shares > 0 && self.borrowed_assets > 0)
            .then(|| self.max_borrow as f64 / self.borrowed_assets as f64)
    }
}

/// Read-only view of Morpho Blue.
pub struct MorphoReader<T: EthCall> {
    transport: T,
    morpho: Address,
}

impl<T: EthCall> MorphoReader<T> {
    pub fn new(transport: T, morpho: Address) -> MorphoReader<T> {
        MorphoReader { transport, morpho }
    }

    pub fn morpho(&self) -> &Address {
        &self.morpho
    }

    /// Market parameters; a wrong id or a wrong Morpho address is an error.
    pub fn params(&self, id: &MarketId) -> Result<MarketParams, ChainError> {
        let r = self.transport.eth_call(
            &self.morpho,
            &call_data(ID_TO_MARKET_PARAMS_SELECTOR, &[id.0]),
        )?;
        decode_market_params(&r)
    }

    pub fn totals(&self, id: &MarketId) -> Result<MarketTotals, ChainError> {
        decode_market(
            &self
                .transport
                .eth_call(&self.morpho, &call_data(MARKET_SELECTOR, &[id.0]))?,
        )
    }

    /// Token decimals, for display only.
    pub fn decimals(&self, token: &Address) -> Result<u8, ChainError> {
        let r = self
            .transport
            .eth_call(token, &call_data(DECIMALS_SELECTOR, &[]))?;
        let d = word_u128(&reply_words(&r, 1)?[0], "decimals")?;
        u8::try_from(d)
            .ok()
            .filter(|d| *d <= 36)
            .ok_or(ChainError::Overflow("decimals"))
    }

    /// Read an account now: position, market totals and the oracle price.
    pub fn position(
        &self,
        id: &MarketId,
        params: &MarketParams,
        user: &Address,
    ) -> Result<Position, ChainError> {
        let raw = decode_position(&self.transport.eth_call(
            &self.morpho,
            &call_data(POSITION_SELECTOR, &[id.0, addr_word(user)]),
        )?)?;
        let totals = self.totals(id)?;
        let price = if raw.borrow_shares == 0 && raw.collateral == 0 {
            0 // nothing to value: do not spend a call
        } else {
            let p = word_u128(
                &reply_words(
                    &self
                        .transport
                        .eth_call(&params.oracle, &call_data(ORACLE_PRICE_SELECTOR, &[]))?,
                    1,
                )?[0],
                "oracle price",
            )?;
            if p == 0 {
                return Err(ChainError::Overflow("oracle price (zero)"));
            }
            p
        };
        Position::new(params, &totals, raw, price)
    }
}

/// An [`AccountSource`] for the generic read-only adapter. Totals are in loan-token units scaled
/// to 6 decimals (NOT dollars: Morpho has no USD price); the threshold is the market LLTV.
pub struct MorphoSource<T: EthCall> {
    reader: MorphoReader<T>,
    id: MarketId,
    params: MarketParams,
    loan_decimals: u8,
    user: Address,
}

impl<T: EthCall> MorphoSource<T> {
    /// Reads the market once so a wrong address or id fails before anything runs.
    pub fn new(
        reader: MorphoReader<T>,
        id: MarketId,
        user: Address,
    ) -> Result<MorphoSource<T>, String> {
        let params = reader.params(&id).map_err(|e| e.to_string())?;
        let loan_decimals = reader
            .decimals(&params.loan_token)
            .map_err(|e| e.to_string())?;
        Ok(MorphoSource {
            reader,
            id,
            params,
            loan_decimals,
            user,
        })
    }
}

fn to_micro(v: u128, decimals: u8) -> Result<u64, String> {
    let micro = if decimals >= 6 {
        v / 10u128.pow(u32::from(decimals - 6))
    } else {
        v.checked_mul(10u128.pow(u32::from(6 - decimals)))
            .ok_or("value is too large")?
    };
    u64::try_from(micro).map_err(|_| "value is too large".to_string())
}

impl<T: EthCall> AccountSource for MorphoSource<T> {
    fn protocol_id(&self) -> &str {
        PROTOCOL_ID
    }

    fn read(&self) -> Result<AggregateView, String> {
        let p = self
            .reader
            .position(&self.id, &self.params, &self.user)
            .map_err(|e| e.to_string())?;
        Ok(AggregateView {
            collateral_micro_usd: to_micro(p.collateral_value, self.loan_decimals)?,
            debt_micro_usd: to_micro(p.borrowed_assets, self.loan_decimals)?,
            liq_threshold_bps: u32::try_from(p.lltv / 100_000_000_000_000).unwrap_or(10_000),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    const MORPHO: &str = "0x2222222222222222222222222222222222222222";
    const USER: &str = "0x1111111111111111111111111111111111111111";
    const LOAN: &str = "0x3333333333333333333333333333333333333333";
    const COLL: &str = "0x4444444444444444444444444444444444444444";
    const ORACLE: &str = "0x5555555555555555555555555555555555555555";
    const IRM: &str = "0x6666666666666666666666666666666666666666";
    const ID: &str = "0xaabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";

    fn addr(s: &str) -> Address {
        Address::parse(s).unwrap()
    }
    fn word(v: u128) -> String {
        format!("{v:064x}")
    }
    fn aword(a: &str) -> String {
        format!("{:0>64}", &a[2..])
    }
    fn id() -> MarketId {
        MarketId::parse(ID).unwrap()
    }
    fn key(sel: [u8; 4], args: &str) -> String {
        format!("0x{}{}", &bytes_to_hex(&sel)[2..], args)
    }

    struct Fake {
        replies: RefCell<BTreeMap<(String, String), String>>,
        calls: Cell<u32>,
    }

    impl Fake {
        /// 80% LLTV; WETH-like 18-dec collateral priced at 3000 USDC (6-dec loan):
        /// price = 3000 * 1e36 * 1e6 / 1e18 = 3e27.
        fn new() -> Fake {
            let f = Fake {
                replies: RefCell::new(BTreeMap::new()),
                calls: Cell::new(0),
            };
            f.set(
                MORPHO,
                key(ID_TO_MARKET_PARAMS_SELECTOR, &ID[2..]),
                format!(
                    "0x{}{}{}{}{}",
                    aword(LOAN),
                    aword(COLL),
                    aword(ORACLE),
                    aword(IRM),
                    word(800_000_000_000_000_000)
                ),
            );
            f.set(
                ORACLE,
                key(ORACLE_PRICE_SELECTOR, ""),
                format!("0x{}", word(3_000_000_000_000_000_000_000_000_000)),
            );
            f.set(LOAN, key(DECIMALS_SELECTOR, ""), format!("0x{}", word(6)));
            // 1000 USDC borrowed in total: 1:1 virtual-free-ish totals
            f.totals(
                1_000_000_000_000,
                1_000_000_000_000_000_000,
                500_000_000_000,
                500_000_000_000_000_000,
            );
            f
        }

        fn set(&self, to: &str, data: String, reply: String) {
            self.replies
                .borrow_mut()
                .insert((to.to_string(), data), reply);
        }

        fn totals(&self, sa: u128, ss: u128, ba: u128, bs: u128) {
            self.set(
                MORPHO,
                key(MARKET_SELECTOR, &ID[2..]),
                format!(
                    "0x{}{}{}{}{}{}",
                    word(sa),
                    word(ss),
                    word(ba),
                    word(bs),
                    word(1),
                    word(0)
                ),
            );
        }

        fn account(self, supply_shares: u128, borrow_shares: u128, collateral: u128) -> Fake {
            self.set(
                MORPHO,
                key(POSITION_SELECTOR, &format!("{}{}", &ID[2..], aword(USER))),
                format!(
                    "0x{}{}{}",
                    word(supply_shares),
                    word(borrow_shares),
                    word(collateral)
                ),
            );
            self
        }
    }

    impl EthCall for Fake {
        fn eth_call(&self, to: &Address, data: &str) -> Result<String, ChainError> {
            self.calls.set(self.calls.get() + 1);
            self.replies
                .borrow()
                .get(&(to.to_hex(), data.to_string()))
                .cloned()
                .ok_or_else(|| ChainError::Transport(format!("unscripted call {to:?} {data}")))
        }
    }

    fn rd(f: Fake) -> MorphoReader<Fake> {
        MorphoReader::new(f, addr(MORPHO))
    }

    fn read(f: Fake) -> Position {
        let r = rd(f);
        let p = r.params(&id()).unwrap();
        r.position(&id(), &p, &addr(USER)).unwrap()
    }

    #[test]
    fn selectors_match_the_documented_signatures() {
        // recomputed with an independent Keccak-256 on 2026-11-26
        for (s, hex) in [
            (POSITION_SELECTOR, "0x93c52062"),
            (MARKET_SELECTOR, "0x5c60e39a"),
            (ID_TO_MARKET_PARAMS_SELECTOR, "0x2c3c9157"),
            (ORACLE_PRICE_SELECTOR, "0xa035b1fe"),
            (DECIMALS_SELECTOR, "0x313ce567"),
        ] {
            assert_eq!(bytes_to_hex(&s), hex);
        }
    }

    #[test]
    fn mul_div_matches_known_wide_vectors() {
        // values computed with Python big integers
        let a = 123_456_789_012_345_678_901_234_567_890_123_456u128;
        let b = 987_654_321_098_765_432_109_876_543_210_987u128;
        let d = 1_000_000_000_000_000_000_000_000_000_000_000_000u128;
        assert_eq!(
            mul_div(a, b, d, false),
            Ok(121_932_631_137_021_795_226_185_032_733_866u128)
        );
        assert_eq!(
            mul_div(a, b, d, true),
            Ok(121_932_631_137_021_795_226_185_032_733_867u128)
        );
        // exact division does not round up
        assert_eq!(mul_div(6, 7, 3, true), Ok(14));
        assert_eq!(mul_div(7, 7, 3, true), Ok(17));
        assert_eq!(mul_div(7, 7, 3, false), Ok(16));
        // result too large, and division by zero
        assert!(mul_div(u128::MAX, u128::MAX, 1, false).is_err());
        assert!(mul_div(1, 1, 0, false).is_err());
        // largest case that still fits
        assert_eq!(
            mul_div(u128::MAX, u128::MAX, u128::MAX, false),
            Ok(u128::MAX)
        );
    }

    #[test]
    fn mul_div_agrees_with_plain_arithmetic_when_it_fits() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let (a, b) = (u128::from(next() >> 8), u128::from(next() >> 8));
            let d = u128::from(next() >> 20) + 1;
            let p = a * b;
            assert_eq!(mul_div(a, b, d, false), Ok(p / d));
            assert_eq!(mul_div(a, b, d, true), Ok(p / d + u128::from(p % d != 0)));
        }
    }

    #[test]
    fn market_id_is_validated() {
        assert!(MarketId::parse(ID).is_ok());
        assert!(MarketId::parse("0x1234").is_err());
        assert!(MarketId::parse(&format!("0x{}", "00".repeat(32))).is_err());
        assert!(MarketId::parse("nonsense").is_err());
        assert_eq!(id().to_hex(), ID);
    }

    #[test]
    fn params_are_decoded_and_validated() {
        let p = rd(Fake::new()).params(&id()).unwrap();
        assert_eq!(p.loan_token, addr(LOAN));
        assert_eq!(p.lltv, 800_000_000_000_000_000);
        // lltv zero or above 100% is refused, as is a missing market (zero loan token)
        let mk = |loan: &str, lltv: u128| {
            format!(
                "0x{}{}{}{}{}",
                aword(loan),
                aword(COLL),
                aword(ORACLE),
                aword(IRM),
                word(lltv)
            )
        };
        assert!(decode_market_params(&mk(LOAN, 0)).is_err());
        assert!(decode_market_params(&mk(LOAN, WAD + 1)).is_err());
        let zero = "0x0000000000000000000000000000000000000000";
        let e = decode_market_params(&mk(zero, WAD / 2)).unwrap_err();
        assert!(e.to_string().contains("does not exist"), "{e}");
        // a zero irm is allowed
        let ok = format!(
            "0x{}{}{}{}{}",
            aword(LOAN),
            aword(COLL),
            aword(ORACLE),
            aword(zero),
            word(WAD / 2)
        );
        assert!(decode_market_params(&ok).is_ok());
        assert!(decode_market_params("0x00").is_err());
    }

    #[test]
    fn a_healthy_borrower_has_the_expected_numbers() {
        // 1 WETH (3000 USDC, lltv 0.8 -> max borrow 2400) with debt shares worth ~1000 USDC
        // totalBorrowAssets 500e6 / totalBorrowShares 500e12 + virtuals; 1000e12 shares ~ 1000e6
        let p = read(Fake::new().account(0, 1_000_000_000_000_000, 1_000_000_000_000_000_000));
        assert_eq!(p.collateral_value, 3_000_000_000);
        assert_eq!(p.max_borrow, 2_400_000_000);
        // 1e15 * (500_000_000_000 + 1) / (500_000_000_000_000_000 + 1e6), rounded up
        let exact = 1_000_000_000_000_000u128 * 500_000_000_001 / 500_000_000_001_000_000;
        assert!(
            p.borrowed_assets == exact || p.borrowed_assets == exact + 1,
            "{}",
            p.borrowed_assets
        );
        assert!(p.healthy());
        let hf = p.health_factor().unwrap();
        assert!(hf > 2.0 && hf < 2.5, "{hf}");
    }

    #[test]
    fn an_underwater_borrower_is_not_healthy() {
        // debt of 2600 USDC against 2400 max
        let p = read(Fake::new().account(0, 2_600_000_000_000_000, 1_000_000_000_000_000_000));
        assert!(p.borrowed_assets > p.max_borrow);
        assert!(!p.healthy());
        assert!(p.health_factor().unwrap() < 1.0);
    }

    #[test]
    fn exactly_at_the_limit_is_healthy() {
        // totals chosen so shares map 1:1 to assets: assets+1 == shares+1e6 is not needed; build
        // totals with virtuals cancelling: totalAssets+1 = totalShares+1e6
        let f = Fake::new();
        f.totals(0, 0, 999_999, 0); // +1 and +1e6 -> ratio exactly 1.0 for borrow
        let p = read(f.account(0, 2_400_000_000, 1_000_000_000_000_000_000));
        assert_eq!(p.borrowed_assets, 2_400_000_000);
        assert_eq!(p.max_borrow, 2_400_000_000);
        assert!(p.healthy(), "maxBorrow >= borrowed must be healthy");
        assert_eq!(p.health_factor(), Some(1.0));
    }

    #[test]
    fn debt_rounds_up_and_collateral_rounds_down() {
        let f = Fake::new();
        f.totals(0, 0, 999_999, 0);
        // one share -> 1 asset exactly; make a case that needs rounding: price tiny
        let p = read(f.account(0, 1, 1));
        assert_eq!(p.borrowed_assets, 1);
        assert_eq!(p.collateral_value, 0); // 1 wei * 3e27 / 1e36 floors to 0
        assert!(!p.healthy());
    }

    #[test]
    fn suppliers_and_empty_accounts_have_no_health_factor() {
        let f = Fake::new().account(5_000_000_000_000, 0, 0);
        let calls_before = f.calls.get();
        let r = rd(f);
        let p0 = r.params(&id()).unwrap();
        let p = r.position(&id(), &p0, &addr(USER)).unwrap();
        assert!(p.healthy() && p.health_factor().is_none());
        assert!(p.supplied_assets > 0);
        // supplier with no debt and no collateral costs no oracle call: params + position + market
        assert_eq!(r.transport.calls.get() - calls_before, 3);
    }

    #[test]
    fn huge_collateral_uses_the_wide_math_instead_of_overflowing() {
        // collateral 1e30 wei * price 3e27 = 3e57 > u128::MAX, but the value 3e21 fits
        let p = read(Fake::new().account(0, 0, 1_000_000_000_000_000_000_000_000_000_000));
        assert_eq!(p.collateral_value, 3_000_000_000_000_000_000_000);
    }

    #[test]
    fn nonsense_from_a_node_is_an_error_not_a_guess() {
        let f = Fake::new().account(0, 1, 1);
        f.set(
            ORACLE,
            key(ORACLE_PRICE_SELECTOR, ""),
            format!("0x{}", word(0)),
        );
        let r = rd(f);
        let p = r.params(&id()).unwrap();
        assert!(r.position(&id(), &p, &addr(USER)).is_err());
        assert!(decode_position("0x").is_err());
        assert!(decode_market(&format!("0x{}", word(1))).is_err());
        // a value wider than 128 bits is refused
        let wide = format!("0x{}{}{}", "ff".repeat(32), word(0), word(0));
        assert!(decode_position(&wide).is_err());
    }

    #[test]
    fn the_source_reads_the_market_once_and_the_account_each_time() {
        // loan 6 decimals; healthy borrower as above
        let f = Fake::new().account(0, 1_000_000_000_000_000, 1_000_000_000_000_000_000);
        let src = MorphoSource::new(rd(f), id(), addr(USER)).unwrap();
        assert_eq!(src.protocol_id(), "morpho");
        let v = src.read().unwrap();
        assert_eq!(v.collateral_micro_usd, 3_000_000_000);
        assert_eq!(v.liq_threshold_bps, 8_000);
        assert!(v.debt_micro_usd > 0);
    }

    #[test]
    fn a_wrong_market_fails_before_anything_runs() {
        struct Empty;
        impl EthCall for Empty {
            fn eth_call(&self, _: &Address, _: &str) -> Result<String, ChainError> {
                Ok("0x".into())
            }
        }
        let r = MorphoReader::new(Empty, addr(MORPHO));
        assert!(MorphoSource::new(r, id(), addr(USER)).is_err());
    }

    #[test]
    fn micro_scaling_handles_any_decimals() {
        assert_eq!(to_micro(1_000_000, 6), Ok(1_000_000));
        assert_eq!(to_micro(10u128.pow(18), 18), Ok(1_000_000));
        assert_eq!(to_micro(5, 0), Ok(5_000_000));
        assert!(to_micro(u128::MAX, 0).is_err());
    }
}
