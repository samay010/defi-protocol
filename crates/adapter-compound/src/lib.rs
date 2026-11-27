//! Read-only Compound III (Comet) account reader.
//!
//! Pure ABI logic on top of [`protocol_chain::EthCall`]: it holds no keys and can only run
//! `eth_call`s. Nothing here signs or sends.
//!
//! Everything below was checked against the Comet source (`compound-finance/comet`, commit
//! `f766f51`, 2026-06-23), not recalled:
//! - read functions: `baseTokenPriceFeed()`, `baseScale()` (a `uint`, `10 ** baseDecimals`),
//!   `numAssets()` (`uint8`), `getAssetInfo(uint8)` (a static struct of eight words:
//!   `offset, asset, priceFeed, scale, borrowCollateralFactor, liquidateCollateralFactor,
//!   liquidationFactor, supplyCap`), `collateralBalanceOf(address,address)` (`uint128`, served
//!   by the extension through the fallback), `borrowBalanceOf(address)`, `balanceOf(address)`,
//!   `getPrice(address)` and `isLiquidatable(address)`. The 4-byte selectors were recomputed
//!   with an independent Keccak-256 and are asserted in the tests.
//! - scales: price feeds have 8 decimals (`PRICE_FEED_DECIMALS = 8`), collateral factors have 18
//!   (`FACTOR_SCALE = 1e18`).
//! - the liquidation rule (`isLiquidatable`): the account is liquidatable iff its base principal
//!   is negative and
//!   `-(debt * basePrice / baseScale) + sum(floor(floor(balance * price / asset.scale) *
//!   liquidateCollateralFactor / 1e18)) < 0`. [`Position::liquidatable`] evaluates exactly that,
//!   and the tests compare it with a line-by-line port of the contract's loop.
//!
//! Known difference from the contract: `isLiquidatable` uses the *stored* interest index while
//! `borrowBalanceOf` accrues interest up to now, so the debt read here can be a little larger
//! than the contract's own check. The health factor shown is therefore slightly conservative.
//!
//! Contract ADDRESSES are never built in: the caller supplies the Comet address from Compound's
//! own deployment list for the chosen testnet.

pub use protocol_chain::ChainError;
use protocol_chain::{bytes_to_hex, hex_to_bytes, Address, EthCall};
use protocol_core::readonly::{AccountSource, AggregateView};

pub const PROTOCOL_ID: &str = "compound";

pub const NUM_ASSETS_SELECTOR: [u8; 4] = [0xa4, 0x6f, 0xe8, 0x3b];
pub const GET_ASSET_INFO_SELECTOR: [u8; 4] = [0xc8, 0xc7, 0xfe, 0x6b];
pub const COLLATERAL_BALANCE_OF_SELECTOR: [u8; 4] = [0x5c, 0x25, 0x49, 0xee];
pub const BORROW_BALANCE_OF_SELECTOR: [u8; 4] = [0x37, 0x4c, 0x49, 0xb4];
pub const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
pub const BASE_TOKEN_PRICE_FEED_SELECTOR: [u8; 4] = [0xe7, 0xda, 0xd6, 0xbd];
pub const BASE_SCALE_SELECTOR: [u8; 4] = [0x44, 0xc1, 0xe5, 0xeb];
pub const GET_PRICE_SELECTOR: [u8; 4] = [0x41, 0x97, 0x6e, 0x09];
pub const IS_LIQUIDATABLE_SELECTOR: [u8; 4] = [0x04, 0x2e, 0x02, 0xcf];

/// `FACTOR_SCALE` in `CometCore.sol`.
pub const FACTOR_SCALE: u128 = 1_000_000_000_000_000_000;
/// Price feed decimals (`PRICE_FEED_DECIMALS`): USD values below are in units of 1e-8 USD.
pub const PRICE_DECIMALS: u32 = 8;
/// A Comet's asset offsets are below 24 (`isInAsset`); more than that is not a Comet.
pub const MAX_ASSETS: u8 = 24;
/// Micro-USD (6 decimals) per 1e-8 USD unit.
const USD8_PER_MICRO: u128 = 100;

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

fn u8_word(v: u8) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[31] = v;
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

fn word_address(w: &[u8; 32]) -> Result<Address, ChainError> {
    if w[..12].iter().any(|b| *b != 0) {
        return Err(ChainError::BadAddress("not a padded address".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&w[12..]);
    if a == [0u8; 20] {
        return Err(ChainError::BadAddress("zero address".into()));
    }
    Ok(Address(a))
}

/// One collateral asset of the market (`getAssetInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    pub offset: u8,
    pub asset: Address,
    pub price_feed: Address,
    /// `10 ** decimals` of the asset.
    pub scale: u64,
    pub borrow_collateral_factor: u64,
    pub liquidate_collateral_factor: u64,
}

pub fn decode_asset_info(reply: &str) -> Result<AssetInfo, ChainError> {
    let w = reply_words(reply, 8)?;
    let small = |i: usize, what: &'static str| -> Result<u64, ChainError> {
        u64::try_from(word_u128(&w[i], what)?).map_err(|_| ChainError::Overflow(what))
    };
    let info = AssetInfo {
        offset: u8::try_from(word_u128(&w[0], "asset offset")?)
            .map_err(|_| ChainError::Overflow("asset offset"))?,
        asset: word_address(&w[1])?,
        price_feed: word_address(&w[2])?,
        scale: small(3, "asset scale")?,
        borrow_collateral_factor: small(4, "borrow collateral factor")?,
        liquidate_collateral_factor: small(5, "liquidate collateral factor")?,
    };
    // w[6] (liquidationFactor) and w[7] (supplyCap) are not needed to read an account
    if info.scale == 0 {
        return Err(ChainError::Overflow("asset scale (zero)"));
    }
    for f in [
        info.borrow_collateral_factor,
        info.liquidate_collateral_factor,
    ] {
        if u128::from(f) > FACTOR_SCALE {
            return Err(ChainError::Overflow("collateral factor above 100%"));
        }
    }
    Ok(info)
}

/// The static part of a market: read once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketConfig {
    pub base_price_feed: Address,
    /// `10 ** baseDecimals`.
    pub base_scale: u128,
    pub assets: Vec<AssetInfo>,
}

/// What one account holds, with prices, all read in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    /// Base token supplied (`balanceOf`), base units.
    pub supplied_base: u128,
    /// Base token owed (`borrowBalanceOf`), base units.
    pub borrowed_base: u128,
    /// Price of the base token, 8 decimals.
    pub base_price: u128,
    /// Per collateral asset (same order as the market): `(balance, price)`; the price is `0`
    /// when the balance is zero (it was not read).
    pub collateral: Vec<(u128, u128)>,
    /// Totals derived from the above, in 1e-8 USD.
    pub debt_usd8: u128,
    pub collateral_usd8: u128,
    pub borrow_capacity_usd8: u128,
    pub liquidation_capacity_usd8: u128,
}

fn mul_div(a: u128, b: u128, d: u128, what: &'static str) -> Result<u128, ChainError> {
    a.checked_mul(b)
        .map(|p| p / d)
        .ok_or(ChainError::Overflow(what))
}

impl Position {
    /// Compute the derived totals exactly as `isLiquidatable` does (floors at each step).
    pub fn new(
        cfg: &MarketConfig,
        supplied_base: u128,
        borrowed_base: u128,
        base_price: u128,
        collateral: Vec<(u128, u128)>,
    ) -> Result<Position, ChainError> {
        if collateral.len() != cfg.assets.len() {
            return Err(ChainError::BadLength(collateral.len()));
        }
        let debt_usd8 = mul_div(borrowed_base, base_price, cfg.base_scale, "debt value")?;
        let (mut total, mut borrow_cap, mut liq_cap) = (0u128, 0u128, 0u128);
        for (info, (balance, price)) in cfg.assets.iter().zip(&collateral) {
            let value = mul_div(*balance, *price, u128::from(info.scale), "collateral value")?;
            let add = |acc: u128, x: u128| acc.checked_add(x).ok_or(ChainError::Overflow("total"));
            total = add(total, value)?;
            borrow_cap = add(
                borrow_cap,
                mul_div(
                    value,
                    u128::from(info.borrow_collateral_factor),
                    FACTOR_SCALE,
                    "borrow capacity",
                )?,
            )?;
            liq_cap = add(
                liq_cap,
                mul_div(
                    value,
                    u128::from(info.liquidate_collateral_factor),
                    FACTOR_SCALE,
                    "liquidation capacity",
                )?,
            )?;
        }
        Ok(Position {
            supplied_base,
            borrowed_base,
            base_price,
            collateral,
            debt_usd8,
            collateral_usd8: total,
            borrow_capacity_usd8: borrow_cap,
            liquidation_capacity_usd8: liq_cap,
        })
    }

    /// `collateral * liquidation factor / debt`; `None` without debt. Below 1 means liquidatable.
    pub fn health_factor(&self) -> Option<f64> {
        (self.debt_usd8 > 0).then(|| self.liquidation_capacity_usd8 as f64 / self.debt_usd8 as f64)
    }

    /// The contract's rule: debt outweighs the liquidation capacity of the collateral.
    pub fn liquidatable(&self) -> bool {
        self.borrowed_base > 0 && self.liquidation_capacity_usd8 < self.debt_usd8
    }

    /// Average liquidation threshold in basis points (the aggregate view's definition).
    pub fn average_threshold_bps(&self) -> u32 {
        if self.collateral_usd8 == 0 {
            return 0;
        }
        let bps = self.liquidation_capacity_usd8.saturating_mul(10_000) / self.collateral_usd8;
        u32::try_from(bps.min(10_000)).unwrap_or(10_000)
    }

    /// Totals in micro-USD for the generic aggregate view.
    pub fn aggregate_view(&self) -> Result<AggregateView, String> {
        let micro = |name: &str, v: u128| {
            u64::try_from(v / USD8_PER_MICRO).map_err(|_| format!("{name} is too large"))
        };
        Ok(AggregateView {
            collateral_micro_usd: micro("total collateral", self.collateral_usd8)?,
            debt_micro_usd: micro("total debt", self.debt_usd8)?,
            liq_threshold_bps: self.average_threshold_bps(),
        })
    }
}

/// Read-only view of one Comet market.
pub struct CompoundReader<T: EthCall> {
    transport: T,
    comet: Address,
}

impl<T: EthCall> CompoundReader<T> {
    pub fn new(transport: T, comet: Address) -> CompoundReader<T> {
        CompoundReader { transport, comet }
    }

    pub fn comet(&self) -> &Address {
        &self.comet
    }

    fn call(&self, data: String) -> Result<String, ChainError> {
        self.transport.eth_call(&self.comet, &data)
    }

    fn call_word(&self, selector: [u8; 4], args: &[[u8; 32]]) -> Result<[u8; 32], ChainError> {
        Ok(reply_words(&self.call(call_data(selector, args))?, 1)?[0])
    }

    fn price(&self, feed: &Address) -> Result<u128, ChainError> {
        let p = word_u128(
            &self.call_word(GET_PRICE_SELECTOR, &[addr_word(feed)])?,
            "price",
        )?;
        if p == 0 {
            return Err(ChainError::Overflow("price (zero)"));
        }
        Ok(p)
    }

    /// Read the market's static configuration (feeds, scales, collateral assets).
    pub fn market(&self) -> Result<MarketConfig, ChainError> {
        let base_price_feed = word_address(&self.call_word(BASE_TOKEN_PRICE_FEED_SELECTOR, &[])?)?;
        let base_scale = word_u128(&self.call_word(BASE_SCALE_SELECTOR, &[])?, "base scale")?;
        if base_scale == 0 || base_scale > 1_000_000_000_000_000_000 {
            return Err(ChainError::Overflow("base scale out of range"));
        }
        let n = word_u128(&self.call_word(NUM_ASSETS_SELECTOR, &[])?, "asset count")?;
        let n = u8::try_from(n)
            .ok()
            .filter(|n| *n <= MAX_ASSETS)
            .ok_or(ChainError::Overflow("asset count"))?;
        let mut assets = Vec::with_capacity(usize::from(n));
        for i in 0..n {
            let info =
                decode_asset_info(&self.call(call_data(GET_ASSET_INFO_SELECTOR, &[u8_word(i)]))?)?;
            assets.push(info);
        }
        Ok(MarketConfig {
            base_price_feed,
            base_scale,
            assets,
        })
    }

    /// Read an account's position now (fresh prices and balances).
    pub fn position(&self, cfg: &MarketConfig, user: &Address) -> Result<Position, ChainError> {
        let who = [addr_word(user)];
        let supplied = word_u128(
            &self.call_word(BALANCE_OF_SELECTOR, &who)?,
            "supplied balance",
        )?;
        let borrowed = word_u128(
            &self.call_word(BORROW_BALANCE_OF_SELECTOR, &who)?,
            "borrow balance",
        )?;
        let base_price = self.price(&cfg.base_price_feed)?;
        let mut collateral = Vec::with_capacity(cfg.assets.len());
        for info in &cfg.assets {
            let balance = word_u128(
                &self.call_word(
                    COLLATERAL_BALANCE_OF_SELECTOR,
                    &[addr_word(user), addr_word(&info.asset)],
                )?,
                "collateral balance",
            )?;
            // an empty asset contributes nothing: do not spend a call on its price
            let price = if balance == 0 {
                0
            } else {
                self.price(&info.price_feed)?
            };
            collateral.push((balance, price));
        }
        Position::new(cfg, supplied, borrowed, base_price, collateral)
    }

    /// The contract's own verdict, to compare with [`Position::liquidatable`].
    pub fn is_liquidatable_on_chain(&self, user: &Address) -> Result<bool, ChainError> {
        let w = self.call_word(IS_LIQUIDATABLE_SELECTOR, &[addr_word(user)])?;
        if w[..31].iter().any(|b| *b != 0) || w[31] > 1 {
            return Err(ChainError::BadHex(
                "isLiquidatable did not return a bool".into(),
            ));
        }
        Ok(w[31] == 1)
    }
}

/// An [`AccountSource`] for the generic read-only adapter: one account in one Comet market.
pub struct CompoundSource<T: EthCall> {
    reader: CompoundReader<T>,
    cfg: MarketConfig,
    user: Address,
}

impl<T: EthCall> CompoundSource<T> {
    /// Reads the market configuration once, so a wrong address fails before anything runs.
    pub fn new(reader: CompoundReader<T>, user: Address) -> Result<CompoundSource<T>, String> {
        let cfg = reader.market().map_err(|e| e.to_string())?;
        Ok(CompoundSource { reader, cfg, user })
    }

    pub fn user(&self) -> &Address {
        &self.user
    }
}

impl<T: EthCall> AccountSource for CompoundSource<T> {
    fn protocol_id(&self) -> &str {
        PROTOCOL_ID
    }

    fn read(&self) -> Result<AggregateView, String> {
        self.reader
            .position(&self.cfg, &self.user)
            .map_err(|e| e.to_string())?
            .aggregate_view()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    const COMET: &str = "0x2222222222222222222222222222222222222222";
    const USER: &str = "0x1111111111111111111111111111111111111111";
    const WETH: &str = "0x3333333333333333333333333333333333333333";
    const WBTC: &str = "0x4444444444444444444444444444444444444444";
    const FEED_BASE: &str = "0x5555555555555555555555555555555555555555";
    const FEED_WETH: &str = "0x6666666666666666666666666666666666666666";
    const FEED_WBTC: &str = "0x7777777777777777777777777777777777777777";

    fn addr(s: &str) -> Address {
        Address::parse(s).unwrap()
    }

    fn word(v: u128) -> String {
        format!("{v:064x}")
    }

    fn aword(a: &str) -> String {
        format!("{:0>64}", &a[2..])
    }

    fn asset_reply(i: u8, asset: &str, feed: &str, scale: u64, bcf: u64, lcf: u64) -> String {
        format!(
            "0x{}{}{}{}{}{}{}{}",
            word(u128::from(i)),
            aword(asset),
            aword(feed),
            word(u128::from(scale)),
            word(u128::from(bcf)),
            word(u128::from(lcf)),
            word(950_000_000_000_000_000), // liquidationFactor (unused)
            word(1_000_000),               // supplyCap (unused)
        )
    }

    /// A scripted Comet: USDC base (6 decimals), WETH (18) and WBTC (8) collateral.
    struct Fake {
        replies: RefCell<BTreeMap<String, String>>,
        calls: Cell<u32>,
    }

    impl Fake {
        fn new() -> Fake {
            let mut m = BTreeMap::new();
            let sel = |s: [u8; 4], args: &str| format!("0x{}{}", &bytes_to_hex(&s)[2..], args);
            let one = |v: String| format!("0x{v}");
            m.insert(
                sel(BASE_TOKEN_PRICE_FEED_SELECTOR, ""),
                one(aword(FEED_BASE)),
            );
            m.insert(sel(BASE_SCALE_SELECTOR, ""), one(word(1_000_000)));
            m.insert(sel(NUM_ASSETS_SELECTOR, ""), one(word(2)));
            m.insert(
                sel(GET_ASSET_INFO_SELECTOR, &word(0)),
                asset_reply(
                    0,
                    WETH,
                    FEED_WETH,
                    1_000_000_000_000_000_000,
                    800_000_000_000_000_000,
                    850_000_000_000_000_000,
                ),
            );
            m.insert(
                sel(GET_ASSET_INFO_SELECTOR, &word(1)),
                asset_reply(
                    1,
                    WBTC,
                    FEED_WBTC,
                    100_000_000,
                    700_000_000_000_000_000,
                    750_000_000_000_000_000,
                ),
            );
            m.insert(
                sel(GET_PRICE_SELECTOR, &aword(FEED_BASE)),
                one(word(100_000_000)),
            );
            m.insert(
                sel(GET_PRICE_SELECTOR, &aword(FEED_WETH)),
                one(word(3_000 * 100_000_000)),
            );
            m.insert(
                sel(GET_PRICE_SELECTOR, &aword(FEED_WBTC)),
                one(word(60_000 * 100_000_000)),
            );
            Fake {
                replies: RefCell::new(m),
                calls: Cell::new(0),
            }
        }

        fn set(&self, selector: [u8; 4], args: &str, reply: &str) {
            self.replies.borrow_mut().insert(
                format!("0x{}{}", &bytes_to_hex(&selector)[2..], args),
                reply.to_string(),
            );
        }

        fn account(self, supplied: u128, borrowed: u128, weth: u128, wbtc: u128) -> Fake {
            self.set(
                BALANCE_OF_SELECTOR,
                &aword(USER),
                &format!("0x{}", word(supplied)),
            );
            self.set(
                BORROW_BALANCE_OF_SELECTOR,
                &aword(USER),
                &format!("0x{}", word(borrowed)),
            );
            self.set(
                COLLATERAL_BALANCE_OF_SELECTOR,
                &format!("{}{}", aword(USER), aword(WETH)),
                &format!("0x{}", word(weth)),
            );
            self.set(
                COLLATERAL_BALANCE_OF_SELECTOR,
                &format!("{}{}", aword(USER), aword(WBTC)),
                &format!("0x{}", word(wbtc)),
            );
            self
        }
    }

    impl EthCall for Fake {
        fn eth_call(&self, to: &Address, data: &str) -> Result<String, ChainError> {
            assert_eq!(to.to_hex(), COMET);
            self.calls.set(self.calls.get() + 1);
            self.replies
                .borrow()
                .get(data)
                .cloned()
                .ok_or_else(|| ChainError::Transport(format!("unscripted call {data}")))
        }
    }

    fn reader(f: Fake) -> CompoundReader<Fake> {
        CompoundReader::new(f, addr(COMET))
    }

    #[test]
    fn selectors_match_the_documented_signatures() {
        // recomputed with an independent Keccak-256 on 2026-11-26
        for (s, hex) in [
            (NUM_ASSETS_SELECTOR, "0xa46fe83b"),
            (GET_ASSET_INFO_SELECTOR, "0xc8c7fe6b"),
            (COLLATERAL_BALANCE_OF_SELECTOR, "0x5c2549ee"),
            (BORROW_BALANCE_OF_SELECTOR, "0x374c49b4"),
            (BALANCE_OF_SELECTOR, "0x70a08231"),
            (BASE_TOKEN_PRICE_FEED_SELECTOR, "0xe7dad6bd"),
            (BASE_SCALE_SELECTOR, "0x44c1e5eb"),
            (GET_PRICE_SELECTOR, "0x41976e09"),
            (IS_LIQUIDATABLE_SELECTOR, "0x042e02cf"),
        ] {
            assert_eq!(bytes_to_hex(&s), hex);
        }
    }

    #[test]
    fn market_config_is_read_and_validated() {
        let r = reader(Fake::new());
        let cfg = r.market().unwrap();
        assert_eq!(cfg.base_price_feed, addr(FEED_BASE));
        assert_eq!(cfg.base_scale, 1_000_000);
        assert_eq!(cfg.assets.len(), 2);
        assert_eq!(cfg.assets[0].asset, addr(WETH));
        assert_eq!(cfg.assets[1].scale, 100_000_000);
        assert_eq!(
            cfg.assets[1].liquidate_collateral_factor,
            750_000_000_000_000_000
        );
    }

    #[test]
    fn a_healthy_borrower_has_the_expected_numbers() {
        // 1 WETH ($3000, liq factor 0.85) backs 2000 USDC of debt
        let r = reader(Fake::new().account(0, 2_000_000_000, 1_000_000_000_000_000_000, 0));
        let cfg = r.market().unwrap();
        let p = r.position(&cfg, &addr(USER)).unwrap();
        assert_eq!(p.debt_usd8, 2_000 * 100_000_000);
        assert_eq!(p.collateral_usd8, 3_000 * 100_000_000);
        assert_eq!(p.liquidation_capacity_usd8, 2_550 * 100_000_000);
        assert_eq!(p.borrow_capacity_usd8, 2_400 * 100_000_000);
        assert!((p.health_factor().unwrap() - 1.275).abs() < 1e-12);
        assert!(!p.liquidatable());
        assert_eq!(p.average_threshold_bps(), 8_500);
    }

    #[test]
    fn an_underwater_borrower_is_liquidatable() {
        let r = reader(Fake::new().account(0, 2_600_000_000, 1_000_000_000_000_000_000, 0));
        let cfg = r.market().unwrap();
        let p = r.position(&cfg, &addr(USER)).unwrap();
        assert!(p.health_factor().unwrap() < 1.0);
        assert!(p.liquidatable());
    }

    #[test]
    fn exactly_at_the_limit_is_not_liquidatable() {
        // capacity 2550 vs debt 2550: liquidity == 0, the contract requires < 0
        let r = reader(Fake::new().account(0, 2_550_000_000, 1_000_000_000_000_000_000, 0));
        let cfg = r.market().unwrap();
        let p = r.position(&cfg, &addr(USER)).unwrap();
        assert_eq!(p.health_factor(), Some(1.0));
        assert!(!p.liquidatable());
    }

    #[test]
    fn suppliers_and_empty_accounts_have_no_health_factor() {
        let r = reader(Fake::new().account(5_000_000, 0, 0, 0));
        let cfg = r.market().unwrap();
        let p = r.position(&cfg, &addr(USER)).unwrap();
        assert!(p.health_factor().is_none() && !p.liquidatable());
        assert_eq!(p.supplied_base, 5_000_000);
        let v = p.aggregate_view().unwrap();
        assert_eq!((v.collateral_micro_usd, v.debt_micro_usd), (0, 0));
    }

    #[test]
    fn empty_collateral_assets_cost_no_price_call() {
        let f = Fake::new().account(0, 1_000_000, 1_000_000_000_000_000_000, 0);
        let r = reader(f);
        let cfg = r.market().unwrap();
        let before = r.transport.calls.get();
        r.position(&cfg, &addr(USER)).unwrap();
        // balanceOf, borrowBalanceOf, base price, 2 balances, 1 price (WETH only) = 6
        assert_eq!(r.transport.calls.get() - before, 6);
    }

    #[test]
    fn two_collateral_assets_add_up() {
        // 1 WETH ($3000) + 0.5 WBTC ($30000): liq capacity 2550 + 22500*... = 3000*.85+30000*.75
        let r =
            reader(Fake::new().account(0, 10_000_000_000, 1_000_000_000_000_000_000, 50_000_000));
        let cfg = r.market().unwrap();
        let p = r.position(&cfg, &addr(USER)).unwrap();
        assert_eq!(p.collateral_usd8, 33_000 * 100_000_000);
        assert_eq!(p.liquidation_capacity_usd8, (2_550 + 22_500) * 100_000_000);
        assert_eq!(p.borrow_capacity_usd8, (2_400 + 21_000) * 100_000_000);
        let v = p.aggregate_view().unwrap();
        assert_eq!(v.collateral_micro_usd, 33_000 * 1_000_000);
        assert_eq!(v.debt_micro_usd, 10_000 * 1_000_000);
        assert_eq!(v.liq_threshold_bps, 7_590); // 25050 / 33000
    }

    /// A line-by-line port of `isLiquidatable` in `CometWithExtendedAssetList.sol`.
    fn contract_is_liquidatable(cfg: &MarketConfig, p: &Position) -> bool {
        // principal < 0 <=> there is debt
        if p.borrowed_base == 0 {
            return false;
        }
        let mut liquidity: i128 = -(p.debt_usd8 as i128);
        for (i, info) in cfg.assets.iter().enumerate() {
            let (balance, price) = p.collateral[i];
            if balance == 0 {
                continue; // not in assetsIn
            }
            if liquidity >= 0 {
                return false;
            }
            let new_amount = balance * price / u128::from(info.scale);
            liquidity +=
                (new_amount * u128::from(info.liquidate_collateral_factor) / FACTOR_SCALE) as i128;
        }
        liquidity < 0
    }

    #[test]
    fn the_rule_matches_a_port_of_the_contract_on_many_random_positions() {
        // deterministic pseudo-random positions (LCG), no external crates
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        let cfg = reader(Fake::new()).market().unwrap();
        let (mut liq, mut ok) = (0, 0);
        for _ in 0..5_000 {
            let weth = u128::from(next(5_000)) * 1_000_000_000_000_000; // 0..5 WETH
            let wbtc = u128::from(next(200)) * 1_000_000; // 0..2 WBTC
            let borrowed = u128::from(next(30_000)) * 1_000_000; // 0..30k USDC
            let p = Position::new(
                &cfg,
                0,
                borrowed,
                100_000_000,
                vec![
                    (weth, if weth > 0 { 3_000 * 100_000_000 } else { 0 }),
                    (wbtc, if wbtc > 0 { 60_000 * 100_000_000 } else { 0 }),
                ],
            )
            .unwrap();
            assert_eq!(
                p.liquidatable(),
                contract_is_liquidatable(&cfg, &p),
                "{p:?}"
            );
            // the health factor and the boolean agree
            match p.health_factor() {
                Some(h) if p.liquidatable() => assert!(h < 1.0, "{h} {p:?}"),
                Some(h) => assert!(h >= 1.0, "{h} {p:?}"),
                None => assert!(!p.liquidatable()),
            }
            if p.liquidatable() {
                liq += 1
            } else {
                ok += 1
            }
        }
        assert!(
            liq > 100 && ok > 100,
            "weak coverage: {liq} liquidatable, {ok} fine"
        );
    }

    #[test]
    fn the_on_chain_verdict_is_read_and_must_be_a_bool() {
        let f = Fake::new();
        f.set(
            IS_LIQUIDATABLE_SELECTOR,
            &aword(USER),
            &format!("0x{}", word(1)),
        );
        assert_eq!(reader(f).is_liquidatable_on_chain(&addr(USER)), Ok(true));
        let f = Fake::new();
        f.set(
            IS_LIQUIDATABLE_SELECTOR,
            &aword(USER),
            &format!("0x{}", word(0)),
        );
        assert_eq!(reader(f).is_liquidatable_on_chain(&addr(USER)), Ok(false));
        let f = Fake::new();
        f.set(
            IS_LIQUIDATABLE_SELECTOR,
            &aword(USER),
            &format!("0x{}", word(2)),
        );
        assert!(reader(f).is_liquidatable_on_chain(&addr(USER)).is_err());
    }

    #[test]
    fn nonsense_from_a_node_is_an_error_not_a_guess() {
        let bad = asset_reply(0, WETH, FEED_WETH, 0, 1, 1);
        assert!(decode_asset_info(&bad).is_err());
        let bad = asset_reply(0, WETH, FEED_WETH, 1_000_000_000_000_000_000, u64::MAX, 1);
        assert!(decode_asset_info(&bad).is_err());
        assert_eq!(decode_asset_info("0x00"), Err(ChainError::BadLength(1)));
        let bad = asset_reply(
            0,
            "0x0000000000000000000000000000000000000000",
            FEED_WETH,
            1,
            1,
            1,
        );
        assert!(decode_asset_info(&bad).is_err());
        let f = Fake::new().account(0, 1_000_000, 1, 0);
        f.set(
            GET_PRICE_SELECTOR,
            &aword(FEED_WETH),
            &format!("0x{}", word(0)),
        );
        let r = reader(f);
        let cfg = r.market().unwrap();
        assert!(r.position(&cfg, &addr(USER)).is_err());
        // values that overflow are reported, never wrapped
        let cfg = reader(Fake::new()).market().unwrap();
        let e = Position::new(
            &cfg,
            0,
            0,
            100_000_000,
            vec![(u128::MAX, 100_000_000), (0, 0)],
        );
        assert!(e.is_err());
        assert!(Position::new(&cfg, 0, 0, 1, vec![]).is_err());
    }

    #[test]
    fn the_source_reads_the_market_once_and_the_account_each_time() {
        let f = Fake::new().account(0, 2_000_000_000, 1_000_000_000_000_000_000, 0);
        let src = CompoundSource::new(reader(f), addr(USER)).unwrap();
        assert_eq!(src.protocol_id(), "compound");
        let after_new = src.reader.transport.calls.get();
        let v = src.read().unwrap();
        assert_eq!(v.collateral_micro_usd, 3_000_000_000);
        assert_eq!(v.debt_micro_usd, 2_000_000_000);
        assert_eq!(v.liq_threshold_bps, 8_500);
        src.read().unwrap();
        // 6 calls per read, none for the market
        assert_eq!(src.reader.transport.calls.get() - after_new, 12);
        assert_eq!(src.user(), &addr(USER));
    }

    #[test]
    fn a_wrong_market_address_fails_before_anything_runs() {
        struct Empty;
        impl EthCall for Empty {
            fn eth_call(&self, _: &Address, _: &str) -> Result<String, ChainError> {
                Ok("0x".into())
            }
        }
        let r = CompoundReader::new(Empty, addr(COMET));
        assert!(CompoundSource::new(r, addr(USER)).is_err());
    }
}
