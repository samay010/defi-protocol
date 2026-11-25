//! Event-driven monitoring of one Aave V3 account (read-only).
//!
//! Instead of polling on a fixed timer, a watcher asks the node for the latest block number and,
//! when it advanced, for the Pool events that touched the account in the blocks since the last
//! look (`eth_getLogs`). Plain HTTP polling: no WebSocket, no subscriptions, no extra
//! dependencies, no keys, nothing signed or sent.
//!
//! Which events, and where the account sits in them (checked 2026-11-25 against a copy of
//! Aave's `IPool.sol`; the topic hashes were recomputed with an independent Keccak-256 and are
//! asserted in the tests):
//! - `Supply(address indexed reserve, address user, address indexed onBehalfOf, uint256 amount,
//!   uint16 indexed referralCode)`: account = topic 2 (`onBehalfOf`)
//! - `Withdraw(address indexed reserve, address indexed user, address indexed to, uint256 amount)`:
//!   account = topic 2 (`user`)
//! - `Borrow(address indexed reserve, address user, address indexed onBehalfOf, uint256 amount,
//!   uint8 interestRateMode, uint256 borrowRate, uint16 indexed referralCode)`: account = topic 2
//! - `Repay(address indexed reserve, address indexed user, address indexed repayer,
//!   uint256 amount, bool useATokens)`: account = topic 2 (`user`)
//! - `LiquidationCall(address indexed collateralAsset, address indexed debtAsset, address
//!   indexed user, uint256 debtToCover, uint256 liquidatedCollateralAmount, address liquidator,
//!   bool receiveAToken)`: account = topic 3 (`user`)
//!
//! Limits, stated plainly: the watcher follows the latest block, so a chain reorganisation can
//! show an event that later disappears (or miss one); a gap larger than [`MAX_BLOCK_RANGE`]
//! blocks between two polls is not backfilled (it is reported as skipped); amounts are shown in
//! raw token units because the event does not carry decimals.

use crate::onchain::{
    bytes_to_hex, hex_to_bytes, AaveReader, Address, ChainError, EthCall, LogQuery, RawLog,
};

pub const SUPPLY_TOPIC: &str = "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61";
pub const WITHDRAW_TOPIC: &str =
    "0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7";
pub const BORROW_TOPIC: &str = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0";
pub const REPAY_TOPIC: &str = "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051";
pub const LIQUIDATION_TOPIC: &str =
    "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";

/// Largest block span asked of the node in one `eth_getLogs` (providers cap ranges).
pub const MAX_BLOCK_RANGE: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Supply,
    Withdraw,
    Borrow,
    Repay,
    Liquidation,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Supply => "supply",
            EventKind::Withdraw => "withdraw",
            EventKind::Borrow => "borrow",
            EventKind::Repay => "repay",
            EventKind::Liquidation => "LIQUIDATION",
        }
    }
}

/// One decoded Pool event that touched the watched account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountEvent {
    pub kind: EventKind,
    pub block: u64,
    pub log_index: Option<u64>,
    pub tx_hash: Option<String>,
    /// The reserve token (for a liquidation: the debt asset that was repaid).
    pub asset: Address,
    /// The collateral asset seized (liquidations only).
    pub collateral_asset: Option<Address>,
    /// Raw token units (for a liquidation: `debtToCover`). `None` if it does not fit `u128`.
    pub amount_raw: Option<u128>,
}

impl AccountEvent {
    pub fn describe(&self) -> String {
        let amount = self
            .amount_raw
            .map(|a| a.to_string())
            .unwrap_or_else(|| "?".into());
        let mut s = format!(
            "{} {} raw units of {} (block {})",
            self.kind.as_str(),
            amount,
            self.asset.to_hex(),
            self.block
        );
        if let Some(c) = &self.collateral_asset {
            s.push_str(&format!(", collateral seized: {}", c.to_hex()));
        }
        if let Some(t) = &self.tx_hash {
            s.push_str(&format!(", tx {t}"));
        }
        s
    }
}

fn padded_address_topic(a: &Address) -> String {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(&a.0);
    bytes_to_hex(&w)
}

/// The two `eth_getLogs` queries that cover every event touching `account` on `pool`.
pub fn account_queries(pool: &Address, account: &Address) -> [LogQuery; 2] {
    let acct = padded_address_topic(account);
    [
        LogQuery {
            address: *pool,
            topics: [
                Some(vec![
                    SUPPLY_TOPIC.into(),
                    WITHDRAW_TOPIC.into(),
                    BORROW_TOPIC.into(),
                    REPAY_TOPIC.into(),
                ]),
                None,
                Some(vec![acct.clone()]),
                None,
            ],
        },
        LogQuery {
            address: *pool,
            topics: [
                Some(vec![LIQUIDATION_TOPIC.into()]),
                None,
                None,
                Some(vec![acct]),
            ],
        },
    ]
}

fn topic_address(topic: &str) -> Result<Address, ChainError> {
    let b = hex_to_bytes(topic)?;
    if b.len() != 32 || b[..12].iter().any(|x| *x != 0) {
        return Err(ChainError::BadHex("topic is not a padded address".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&b[12..]);
    Ok(Address(a))
}

/// Word `i` of the log data as `u128`, `None` if it does not fit.
fn data_word_u128(data: &[u8], i: usize) -> Result<Option<u128>, ChainError> {
    let w = data
        .get(i * 32..(i + 1) * 32)
        .ok_or(ChainError::BadLength(data.len()))?;
    if w[..16].iter().any(|b| *b != 0) {
        return Ok(None);
    }
    let mut v = [0u8; 16];
    v.copy_from_slice(&w[16..]);
    Ok(Some(u128::from_be_bytes(v)))
}

/// Decode one raw log into an [`AccountEvent`]. Anything unexpected is an error, never a guess.
pub fn decode_event(log: &RawLog) -> Result<AccountEvent, ChainError> {
    let t0 = log
        .topics
        .first()
        .ok_or_else(|| ChainError::BadHex("log without topics".into()))?
        .to_ascii_lowercase();
    let data = hex_to_bytes(&log.data)?;
    let topic = |i: usize| {
        log.topics
            .get(i)
            .ok_or_else(|| ChainError::BadHex(format!("log has no topic {i}")))
    };
    let (kind, asset, collateral, amount_word, min_words) = match t0.as_str() {
        SUPPLY_TOPIC => (EventKind::Supply, topic_address(topic(1)?)?, None, 1, 2),
        WITHDRAW_TOPIC => (EventKind::Withdraw, topic_address(topic(1)?)?, None, 0, 1),
        BORROW_TOPIC => (EventKind::Borrow, topic_address(topic(1)?)?, None, 1, 4),
        REPAY_TOPIC => (EventKind::Repay, topic_address(topic(1)?)?, None, 0, 2),
        LIQUIDATION_TOPIC => (
            EventKind::Liquidation,
            topic_address(topic(2)?)?,
            Some(topic_address(topic(1)?)?),
            0,
            4,
        ),
        _ => return Err(ChainError::BadHex("unknown event".into())),
    };
    if data.len() < min_words * 32 {
        return Err(ChainError::BadLength(data.len()));
    }
    Ok(AccountEvent {
        kind,
        block: log.block_number,
        log_index: log.log_index,
        tx_hash: log.tx_hash.clone(),
        asset,
        collateral_asset: collateral,
        amount_raw: data_word_u128(&data, amount_word)?,
    })
}

/// What one poll found.
#[derive(Debug, PartialEq, Eq)]
pub struct Poll {
    /// The latest block the node reported.
    pub block: u64,
    /// True when `block` is newer than the previous poll (always true on the first poll).
    pub advanced: bool,
    /// Account events in the blocks since the previous poll, oldest first.
    pub events: Vec<AccountEvent>,
    /// Blocks between the previous poll and this one that were not searched (gap too large).
    pub skipped_blocks: u64,
}

/// Follows the latest block and the account's Pool events. The first poll only records where
/// "now" is: history before the watcher started is not reported.
pub struct AccountWatcher<T: EthCall> {
    reader: AaveReader<T>,
    account: Address,
    last_block: Option<u64>,
}

impl<T: EthCall> AccountWatcher<T> {
    pub fn new(reader: AaveReader<T>, account: Address) -> AccountWatcher<T> {
        AccountWatcher {
            reader,
            account,
            last_block: None,
        }
    }

    pub fn poll(&mut self) -> Result<Poll, ChainError> {
        let head = self.reader.block_number()?;
        let Some(last) = self.last_block else {
            self.last_block = Some(head);
            return Ok(Poll {
                block: head,
                advanced: true,
                events: Vec::new(),
                skipped_blocks: 0,
            });
        };
        if head <= last {
            return Ok(Poll {
                block: head.max(last),
                advanced: false,
                events: Vec::new(),
                skipped_blocks: 0,
            });
        }
        let wanted_from = last + 1;
        let from = wanted_from.max(head.saturating_sub(MAX_BLOCK_RANGE - 1));
        let skipped = from - wanted_from;
        let mut events = Vec::new();
        for q in account_queries(self.reader.pool(), &self.account) {
            for log in self.reader.logs(&q, from, head)? {
                events.push(decode_event(&log)?);
            }
        }
        events.sort_by_key(|e| (e.block, e.log_index.unwrap_or(0)));
        // advance only after everything succeeded: a failed poll is retried over the same range
        self.last_block = Some(head);
        Ok(Poll {
            block: head,
            advanced: true,
            events,
            skipped_blocks: skipped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const POOL: &str = "0x6ae43d3271ff6888e7fc43fd7321a503ff738951";
    const ME: &str = "0x2222222222222222222222222222222222222222";
    const TOKEN: &str = "0x1111111111111111111111111111111111111111";
    const OTHER: &str = "0x3333333333333333333333333333333333333333";

    fn addr(s: &str) -> Address {
        Address::parse(s).unwrap()
    }

    fn word(v: u128) -> Vec<u8> {
        let mut w = vec![0u8; 16];
        w.extend_from_slice(&v.to_be_bytes());
        w
    }

    fn topic_of(a: &str) -> String {
        padded_address_topic(&addr(a))
    }

    fn log(topic0: &str, topics: &[&str], words: &[u128], block: u64) -> RawLog {
        let mut t = vec![topic0.to_string()];
        t.extend(topics.iter().map(|s| s.to_string()));
        RawLog {
            block_number: block,
            log_index: Some(1),
            tx_hash: Some("0xabc".into()),
            topics: t,
            data: bytes_to_hex(&words.iter().flat_map(|w| word(*w)).collect::<Vec<u8>>()),
        }
    }

    #[test]
    fn topic_hashes_are_the_documented_event_signatures() {
        // recomputed with an independent Keccak-256 on 2026-11-25
        assert_eq!(SUPPLY_TOPIC.len(), 66);
        assert_eq!(
            SUPPLY_TOPIC,
            "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61"
        );
        for t in [
            SUPPLY_TOPIC,
            WITHDRAW_TOPIC,
            BORROW_TOPIC,
            REPAY_TOPIC,
            LIQUIDATION_TOPIC,
        ] {
            assert_eq!(t.len(), 66);
            assert!(t.starts_with("0x") && t[2..].bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(t, t.to_ascii_lowercase());
        }
    }

    #[test]
    fn queries_put_the_account_where_each_event_has_it() {
        let [a, b] = account_queries(&addr(POOL), &addr(ME));
        assert_eq!(a.address, addr(POOL));
        assert_eq!(
            a.topics[0].as_ref().unwrap(),
            &vec![
                SUPPLY_TOPIC.to_string(),
                WITHDRAW_TOPIC.to_string(),
                BORROW_TOPIC.to_string(),
                REPAY_TOPIC.to_string()
            ]
        );
        assert_eq!(a.topics[2].as_ref().unwrap(), &vec![topic_of(ME)]);
        assert!(a.topics[1].is_none() && a.topics[3].is_none());
        assert_eq!(
            b.topics[0].as_ref().unwrap(),
            &vec![LIQUIDATION_TOPIC.to_string()]
        );
        assert_eq!(b.topics[3].as_ref().unwrap(), &vec![topic_of(ME)]);
        assert!(b.topics[1].is_none() && b.topics[2].is_none());
    }

    #[test]
    fn each_event_kind_decodes_with_the_right_fields() {
        let supply = decode_event(&log(
            SUPPLY_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(OTHER)],
            &[7, 1_500_000],
            10,
        ))
        .unwrap();
        assert_eq!(supply.kind, EventKind::Supply);
        assert_eq!(supply.asset, addr(TOKEN));
        assert_eq!(supply.amount_raw, Some(1_500_000));
        assert_eq!(supply.block, 10);

        let withdraw = decode_event(&log(
            WITHDRAW_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(ME)],
            &[42],
            11,
        ))
        .unwrap();
        assert_eq!(withdraw.kind, EventKind::Withdraw);
        assert_eq!(withdraw.amount_raw, Some(42));

        let borrow = decode_event(&log(
            BORROW_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), "0x0"],
            &[7, 900, 2, 5],
            12,
        ));
        // topic 3 (referral) is not read, so a short one is fine; the amount is data word 1
        assert_eq!(borrow.unwrap().amount_raw, Some(900));

        let repay = decode_event(&log(
            REPAY_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(OTHER)],
            &[33, 0],
            13,
        ))
        .unwrap();
        assert_eq!(repay.kind, EventKind::Repay);
        assert_eq!(repay.amount_raw, Some(33));

        let liq = decode_event(&log(
            LIQUIDATION_TOPIC,
            &[&topic_of(OTHER), &topic_of(TOKEN), &topic_of(ME)],
            &[1000, 2000, 9, 0],
            14,
        ))
        .unwrap();
        assert_eq!(liq.kind, EventKind::Liquidation);
        assert_eq!(liq.asset, addr(TOKEN), "debt asset");
        assert_eq!(liq.collateral_asset, Some(addr(OTHER)));
        assert_eq!(liq.amount_raw, Some(1000));
        assert!(liq.describe().contains("LIQUIDATION"));
    }

    #[test]
    fn malformed_logs_are_errors_never_guesses() {
        let good = log(
            SUPPLY_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(OTHER)],
            &[7, 5],
            1,
        );
        let mut no_topics = good.clone();
        no_topics.topics.clear();
        assert!(decode_event(&no_topics).is_err());
        let mut unknown = good.clone();
        unknown.topics[0] = format!("0x{}", "ab".repeat(32));
        assert!(decode_event(&unknown).is_err());
        let mut short = good.clone();
        short.data = bytes_to_hex(&word(7));
        assert!(decode_event(&short).is_err());
        let mut bad_topic = good.clone();
        bad_topic.topics[1] = format!("0x{}", "ff".repeat(32));
        assert!(decode_event(&bad_topic).is_err());
        let mut bad_hex = good;
        bad_hex.data = "nothex".into();
        assert!(decode_event(&bad_hex).is_err());
        // an amount above u128 is reported as unknown, not truncated
        let mut huge = log(
            WITHDRAW_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(ME)],
            &[1],
            1,
        );
        huge.data = format!("0x{}", "ff".repeat(32));
        assert_eq!(decode_event(&huge).unwrap().amount_raw, None);
    }

    struct Fake {
        heads: RefCell<Vec<Result<u64, String>>>,
        logs: RefCell<Vec<RawLog>>,
        queries: RefCell<Vec<(u64, u64)>>,
    }

    impl Fake {
        fn new(heads: Vec<Result<u64, String>>, logs: Vec<RawLog>) -> Fake {
            Fake {
                heads: RefCell::new(heads),
                logs: RefCell::new(logs),
                queries: RefCell::new(Vec::new()),
            }
        }
    }

    impl EthCall for Fake {
        fn eth_call(&self, _: &Address, _: &str) -> Result<String, ChainError> {
            unreachable!("the watcher never calls contracts")
        }
        fn block_number(&self) -> Result<u64, ChainError> {
            self.heads
                .borrow_mut()
                .remove(0)
                .map_err(ChainError::Transport)
        }
        fn get_logs(&self, q: &LogQuery, from: u64, to: u64) -> Result<Vec<RawLog>, ChainError> {
            self.queries.borrow_mut().push((from, to));
            // hand each log to the query whose topic0 list contains it, once
            let first = q.topics[0].as_ref().unwrap();
            let mut all = self.logs.borrow_mut();
            let (mine, rest): (Vec<RawLog>, Vec<RawLog>) = all
                .drain(..)
                .partition(|l| first.iter().any(|t| *t == l.topics[0]));
            *all = rest;
            Ok(mine
                .into_iter()
                .filter(|l| l.block_number >= from && l.block_number <= to)
                .collect())
        }
    }

    fn watcher(heads: Vec<Result<u64, String>>, logs: Vec<RawLog>) -> AccountWatcher<Fake> {
        AccountWatcher::new(
            AaveReader::new(Fake::new(heads, logs), addr(POOL)),
            addr(ME),
        )
    }

    fn q(w: &AccountWatcher<Fake>) -> Vec<(u64, u64)> {
        w.reader.transport_for_tests().queries.borrow().clone()
    }

    #[test]
    fn the_first_poll_only_marks_now_and_reports_no_history() {
        let mut w = watcher(vec![Ok(100)], vec![]);
        let p = w.poll().unwrap();
        assert_eq!(
            p,
            Poll {
                block: 100,
                advanced: true,
                events: vec![],
                skipped_blocks: 0
            }
        );
        assert!(q(&w).is_empty(), "no log query for history");
    }

    #[test]
    fn no_new_block_means_no_query() {
        let mut w = watcher(vec![Ok(100), Ok(100), Ok(99)], vec![]);
        w.poll().unwrap();
        let p = w.poll().unwrap();
        assert!(!p.advanced && p.events.is_empty());
        assert!(!w.poll().unwrap().advanced, "a node behind us is not new");
        assert!(q(&w).is_empty());
    }

    #[test]
    fn new_blocks_query_exactly_the_new_range_and_sort_events() {
        let late = log(
            REPAY_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(OTHER)],
            &[5, 0],
            103,
        );
        let early = log(
            SUPPLY_TOPIC,
            &[&topic_of(TOKEN), &topic_of(ME), &topic_of(OTHER)],
            &[7, 9],
            101,
        );
        let liq = log(
            LIQUIDATION_TOPIC,
            &[&topic_of(OTHER), &topic_of(TOKEN), &topic_of(ME)],
            &[1, 2, 3, 0],
            102,
        );
        let mut w = watcher(vec![Ok(100), Ok(103)], vec![late, early, liq]);
        w.poll().unwrap();
        let p = w.poll().unwrap();
        assert!(p.advanced && p.skipped_blocks == 0);
        let blocks: Vec<u64> = p.events.iter().map(|e| e.block).collect();
        assert_eq!(blocks, vec![101, 102, 103]);
        assert_eq!(q(&w), vec![(101, 103), (101, 103)], "one query per filter");
    }

    #[test]
    fn a_huge_gap_is_clamped_and_reported_not_backfilled() {
        let mut w = watcher(vec![Ok(100), Ok(10_000)], vec![]);
        w.poll().unwrap();
        let p = w.poll().unwrap();
        assert_eq!(p.skipped_blocks, 10_000 - (MAX_BLOCK_RANGE - 1) - 101);
        assert_eq!(q(&w)[0], (10_000 - MAX_BLOCK_RANGE + 1, 10_000));
    }

    #[test]
    fn a_failed_poll_is_retried_over_the_same_range() {
        let mut w = watcher(vec![Ok(100), Err("rpc down".into()), Ok(105)], vec![]);
        w.poll().unwrap();
        assert!(w.poll().is_err());
        let p = w.poll().unwrap();
        assert!(p.advanced);
        assert_eq!(q(&w)[0], (101, 105), "nothing was skipped by the failure");
    }

    #[test]
    fn a_transport_without_logs_says_so() {
        struct Plain;
        impl EthCall for Plain {
            fn eth_call(&self, _: &Address, _: &str) -> Result<String, ChainError> {
                Ok("0x".into())
            }
        }
        let mut w = AccountWatcher::new(AaveReader::new(Plain, addr(POOL)), addr(ME));
        let e = w.poll().unwrap_err();
        assert!(e.to_string().contains("cannot read blocks"), "{e}");
    }
}
