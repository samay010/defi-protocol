//! Preflight simulation of Aave V3 Pool actions with a read-only `eth_call`.
//!
//! This builds the calldata a real `supply`, `withdraw`, `borrow` or `repay` would carry and asks
//! a node to *execute it without sending anything*, as if the account had sent it, against the
//! latest block. The answer is either "would succeed" or "would revert" with the reason. It
//! holds no keys and cannot sign or broadcast: the only transport capability is `eth_call`.
//!
//! What it tells you: whether the action would be accepted by the Pool right now (balances,
//! allowances, borrow capacity, caps, paused markets). What it does not tell you: that the
//! action is a good idea, that state will not change before a real transaction, or what it
//! would cost in gas.
//!
//! Sources: function signatures, parameter order, `interestRateMode = 2` (variable; stable is
//! deprecated) and `referralCode = 0` are from the official Aave docs ("Pool" page, checked
//! 2026-11-25). The 4-byte selectors were recomputed with an independent Keccak-256 and are
//! asserted in the tests below. Asset ADDRESSES are never built in: the caller supplies them,
//! copied from Aave's official address book.

use crate::onchain::{bytes_to_hex, hex_to_bytes, AaveReader, Address, ChainError, EthCall};

/// `supply(address,uint256,address,uint16)`
pub const SUPPLY_SELECTOR: [u8; 4] = [0x61, 0x7b, 0xa0, 0x37];
/// `withdraw(address,uint256,address)`
pub const WITHDRAW_SELECTOR: [u8; 4] = [0x69, 0x32, 0x8d, 0xec];
/// `borrow(address,uint256,uint256,uint16,address)`
pub const BORROW_SELECTOR: [u8; 4] = [0xa4, 0x15, 0xbc, 0xad];
/// `repay(address,uint256,uint256,address)`
pub const REPAY_SELECTOR: [u8; 4] = [0x57, 0x3a, 0xde, 0x81];
/// ERC-20 `decimals()`
pub const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];
/// Solidity `Error(string)`, how a plain `revert("...")` is reported.
const ERROR_STRING_SELECTOR: &str = "08c379a0";

/// Variable interest rate mode: the only one Aave still accepts.
const VARIABLE_RATE_MODE: u128 = 2;
/// Largest number of decimals we accept for a token (keeps amounts inside `u128`).
pub const MAX_DECIMALS: u8 = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Supply,
    Withdraw,
    Borrow,
    Repay,
}

impl Operation {
    pub fn parse(s: &str) -> Option<Operation> {
        match s.trim().to_ascii_lowercase().as_str() {
            "supply" => Some(Operation::Supply),
            "withdraw" => Some(Operation::Withdraw),
            "borrow" => Some(Operation::Borrow),
            "repay" => Some(Operation::Repay),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Supply => "supply",
            Operation::Withdraw => "withdraw",
            Operation::Borrow => "borrow",
            Operation::Repay => "repay",
        }
    }
}

fn word_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_addr(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(&a.0);
    w
}

/// Calldata (`0x` hex) for `operation` on `asset`, for `amount` raw token units, where the
/// account is both the sender and the beneficiary (`onBehalfOf` / `to`).
pub fn encode_operation(op: Operation, asset: &Address, amount: u128, account: &Address) -> String {
    let mut d: Vec<u8> = Vec::with_capacity(4 + 5 * 32);
    match op {
        Operation::Supply => {
            d.extend_from_slice(&SUPPLY_SELECTOR);
            d.extend_from_slice(&word_addr(asset));
            d.extend_from_slice(&word_u128(amount));
            d.extend_from_slice(&word_addr(account)); // onBehalfOf
            d.extend_from_slice(&word_u128(0)); // referralCode
        }
        Operation::Withdraw => {
            d.extend_from_slice(&WITHDRAW_SELECTOR);
            d.extend_from_slice(&word_addr(asset));
            d.extend_from_slice(&word_u128(amount));
            d.extend_from_slice(&word_addr(account)); // to
        }
        Operation::Borrow => {
            d.extend_from_slice(&BORROW_SELECTOR);
            d.extend_from_slice(&word_addr(asset));
            d.extend_from_slice(&word_u128(amount));
            d.extend_from_slice(&word_u128(VARIABLE_RATE_MODE));
            d.extend_from_slice(&word_u128(0)); // referralCode
            d.extend_from_slice(&word_addr(account)); // onBehalfOf
        }
        Operation::Repay => {
            d.extend_from_slice(&REPAY_SELECTOR);
            d.extend_from_slice(&word_addr(asset));
            d.extend_from_slice(&word_u128(amount));
            d.extend_from_slice(&word_u128(VARIABLE_RATE_MODE));
            d.extend_from_slice(&word_addr(account)); // onBehalfOf
        }
    }
    bytes_to_hex(&d)
}

/// Split a plain decimal string ("10", "0.5", "12.345") into integer and fraction digits.
/// No sign, no exponent, no thousands separators, digits on both sides of the point.
fn split_amount(text: &str) -> Result<(&str, &str), String> {
    let bad = || format!("bad amount '{text}': use a plain number like 10 or 0.5");
    let (int, frac) = match text.split_once('.') {
        Some((i, f)) => (i, f),
        None => (text, ""),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) {
        return Err(bad());
    }
    if text.contains('.') && frac.is_empty() {
        return Err(bad());
    }
    Ok((int, frac))
}

/// Syntax check only (no token decimals needed): lets the CLI reject typos before any network.
pub fn check_amount_syntax(text: &str) -> Result<(), String> {
    split_amount(text).map(|_| ())
}

/// Parse a decimal amount into raw token units for a token with `decimals` decimals.
/// Never rounds: more fractional digits than the token has is an error. Zero is refused.
pub fn parse_amount(text: &str, decimals: u8) -> Result<u128, String> {
    if decimals > MAX_DECIMALS {
        return Err(format!(
            "token reports {decimals} decimals (max {MAX_DECIMALS})"
        ));
    }
    let (int, frac) = split_amount(text)?;
    if frac.len() > decimals as usize {
        return Err(format!(
            "'{text}' has more than {decimals} decimal places, which this token cannot represent"
        ));
    }
    let too_big = || format!("amount '{text}' is too large");
    let mut raw: u128 = 0;
    for b in int.bytes().chain(frac.bytes()) {
        raw = raw
            .checked_mul(10)
            .and_then(|r| r.checked_add(u128::from(b - b'0')))
            .ok_or_else(too_big)?;
    }
    for _ in frac.len()..decimals as usize {
        raw = raw.checked_mul(10).ok_or_else(too_big)?;
    }
    if raw == 0 {
        return Err("amount must be greater than zero".into());
    }
    Ok(raw)
}

/// Decode an ERC-20 `decimals()` reply: one 32-byte word holding a small number.
pub fn decode_decimals(reply_hex: &str) -> Result<u8, ChainError> {
    let b = hex_to_bytes(reply_hex)?;
    if b.len() != 32 {
        return Err(ChainError::BadLength(b.len()));
    }
    if b[..31].iter().any(|x| *x != 0) || b[31] > MAX_DECIMALS {
        return Err(ChainError::Overflow("token decimals"));
    }
    Ok(b[31])
}

/// Read a token's `decimals()`.
pub fn token_decimals<T: EthCall>(
    reader: &AaveReader<T>,
    token: &Address,
) -> Result<u8, ChainError> {
    let reply = reader.call_from(None, token, &bytes_to_hex(&DECIMALS_SELECTOR))?;
    decode_decimals(&reply)
}

/// What the node said about the call.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The call would have succeeded; `returned` is the raw return data (`0x` hex).
    WouldSucceed { returned: String },
    /// The call would have reverted; `reason` is the decoded revert string if there was one,
    /// otherwise the node's own message (with the RPC URL already removed).
    WouldRevert { reason: String },
}

/// Pull a readable revert string out of a node error message that carries the revert data
/// (`...data: "0x08c379a0..."`). `None` when there is no standard `Error(string)` payload.
pub fn revert_reason_from_message(msg: &str) -> Option<String> {
    let start = msg.find(&format!("0x{ERROR_STRING_SELECTOR}"))?;
    let hex: String = msg[start..]
        .chars()
        .take_while(|c| *c == 'x' || c.is_ascii_hexdigit())
        .collect();
    let bytes = hex_to_bytes(&hex).ok()?;
    // selector + offset word + length word + string bytes
    if bytes.len() < 4 + 64 {
        return None;
    }
    let body = &bytes[4..];
    if body[..31].iter().any(|b| *b != 0) || body[31] != 32 {
        return None;
    }
    if body[32..60].iter().any(|b| *b != 0) {
        return None;
    }
    let len = u32::from_be_bytes([body[60], body[61], body[62], body[63]]) as usize;
    let s = body.get(64..64 + len)?;
    let text = String::from_utf8_lossy(s);
    Some(
        text.chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect(),
    )
}

fn looks_like_revert(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("revert")
}

/// Simulate `op` for `account` against the reader's Pool. A revert is a normal answer
/// (`Outcome::WouldRevert`); only a failure to ask (network, bad reply) is an `Err`.
pub fn simulate<T: EthCall>(
    reader: &AaveReader<T>,
    op: Operation,
    asset: &Address,
    amount: u128,
    account: &Address,
) -> Result<Outcome, ChainError> {
    let data = encode_operation(op, asset, amount, account);
    match reader.call_from(Some(account), reader.pool(), &data) {
        Ok(returned) => Ok(Outcome::WouldSucceed { returned }),
        Err(ChainError::Transport(msg)) if looks_like_revert(&msg) => {
            let reason = revert_reason_from_message(&msg).unwrap_or(msg);
            Ok(Outcome::WouldRevert { reason })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const POOL: &str = "0x6ae43d3271ff6888e7fc43fd7321a503ff738951";
    const ASSET: &str = "0x1111111111111111111111111111111111111111";
    const ACCT: &str = "0x2222222222222222222222222222222222222222";

    fn addr(s: &str) -> Address {
        Address::parse(s).unwrap()
    }

    fn bytes(hex: &str) -> Vec<u8> {
        hex_to_bytes(hex).unwrap()
    }

    #[test]
    fn selectors_match_the_documented_signatures() {
        // verified with an independent Keccak-256 on 2026-11-25
        assert_eq!(bytes_to_hex(&SUPPLY_SELECTOR), "0x617ba037");
        assert_eq!(bytes_to_hex(&WITHDRAW_SELECTOR), "0x69328dec");
        assert_eq!(bytes_to_hex(&BORROW_SELECTOR), "0xa415bcad");
        assert_eq!(bytes_to_hex(&REPAY_SELECTOR), "0x573ade81");
        assert_eq!(bytes_to_hex(&DECIMALS_SELECTOR), "0x313ce567");
    }

    #[test]
    fn operation_names_round_trip() {
        for op in [
            Operation::Supply,
            Operation::Withdraw,
            Operation::Borrow,
            Operation::Repay,
        ] {
            assert_eq!(Operation::parse(op.as_str()), Some(op));
        }
        assert_eq!(Operation::parse("SUPPLY"), Some(Operation::Supply));
        assert_eq!(Operation::parse("swap"), None);
    }

    fn words(data: &[u8]) -> Vec<&[u8]> {
        data[4..].chunks(32).collect()
    }

    fn tail20(w: &[u8]) -> &[u8] {
        assert!(w[..12].iter().all(|b| *b == 0), "address word not padded");
        &w[12..]
    }

    fn as_u128(w: &[u8]) -> u128 {
        assert!(w[..16].iter().all(|b| *b == 0), "value does not fit u128");
        u128::from_be_bytes(w[16..].try_into().unwrap())
    }

    #[test]
    fn supply_layout() {
        let d = bytes(&encode_operation(
            Operation::Supply,
            &addr(ASSET),
            1_500_000,
            &addr(ACCT),
        ));
        assert_eq!(&d[..4], &SUPPLY_SELECTOR);
        let w = words(&d);
        assert_eq!(w.len(), 4);
        assert_eq!(tail20(w[0]), &addr(ASSET).0);
        assert_eq!(as_u128(w[1]), 1_500_000);
        assert_eq!(tail20(w[2]), &addr(ACCT).0);
        assert_eq!(as_u128(w[3]), 0);
    }

    #[test]
    fn withdraw_layout() {
        let d = bytes(&encode_operation(
            Operation::Withdraw,
            &addr(ASSET),
            7,
            &addr(ACCT),
        ));
        assert_eq!(&d[..4], &WITHDRAW_SELECTOR);
        let w = words(&d);
        assert_eq!(w.len(), 3);
        assert_eq!(tail20(w[0]), &addr(ASSET).0);
        assert_eq!(as_u128(w[1]), 7);
        assert_eq!(tail20(w[2]), &addr(ACCT).0);
    }

    #[test]
    fn borrow_layout_uses_variable_rate_and_no_referral() {
        let d = bytes(&encode_operation(
            Operation::Borrow,
            &addr(ASSET),
            9,
            &addr(ACCT),
        ));
        assert_eq!(&d[..4], &BORROW_SELECTOR);
        let w = words(&d);
        assert_eq!(w.len(), 5);
        assert_eq!(tail20(w[0]), &addr(ASSET).0);
        assert_eq!(as_u128(w[1]), 9);
        assert_eq!(as_u128(w[2]), 2, "interestRateMode must be variable (2)");
        assert_eq!(as_u128(w[3]), 0, "referralCode");
        assert_eq!(tail20(w[4]), &addr(ACCT).0);
    }

    #[test]
    fn repay_layout_uses_variable_rate() {
        let d = bytes(&encode_operation(
            Operation::Repay,
            &addr(ASSET),
            3,
            &addr(ACCT),
        ));
        assert_eq!(&d[..4], &REPAY_SELECTOR);
        let w = words(&d);
        assert_eq!(w.len(), 4);
        assert_eq!(as_u128(w[1]), 3);
        assert_eq!(as_u128(w[2]), 2);
        assert_eq!(tail20(w[3]), &addr(ACCT).0);
    }

    #[test]
    fn amounts_parse_exactly_and_never_round() {
        assert_eq!(parse_amount("10", 6), Ok(10_000_000));
        assert_eq!(parse_amount("0.5", 6), Ok(500_000));
        assert_eq!(parse_amount("1.234567", 6), Ok(1_234_567));
        assert_eq!(parse_amount("0.000001", 6), Ok(1));
        assert_eq!(parse_amount("1", 18), Ok(1_000_000_000_000_000_000));
        assert_eq!(parse_amount("5", 0), Ok(5));
        assert!(parse_amount("1.2345678", 6)
            .unwrap_err()
            .contains("decimal places"));
        assert!(parse_amount("0.5", 0).is_err());
    }

    #[test]
    fn bad_amounts_are_refused() {
        for bad in [
            "", "0", "0.0", "-1", "+1", "1e3", "1.2.3", ".", ".5", "1.", " 1", "1 ", "0x10",
            "1,000", "abc",
        ] {
            assert!(parse_amount(bad, 6).is_err(), "accepted {bad:?}");
        }
        // overflow is an error, never a wrap
        assert!(parse_amount("340282366920938463463374607431768211456", 0).is_err());
        assert!(parse_amount("1000000", 36).is_err());
        assert!(parse_amount("1", 37).is_err());
        assert!(check_amount_syntax("12.5").is_ok());
        assert!(check_amount_syntax("1.2.3").is_err());
    }

    #[test]
    fn decimals_reply_is_strict() {
        let w = |last: u8, first: u8| {
            let mut v = vec![0u8; 32];
            v[0] = first;
            v[31] = last;
            bytes_to_hex(&v)
        };
        assert_eq!(decode_decimals(&w(6, 0)), Ok(6));
        assert_eq!(decode_decimals(&w(18, 0)), Ok(18));
        assert!(decode_decimals(&w(255, 0)).is_err());
        assert!(decode_decimals(&w(6, 1)).is_err());
        assert!(decode_decimals("0x00").is_err());
        assert!(decode_decimals("nothex").is_err());
    }

    fn error_string_payload(s: &str) -> String {
        let mut d = vec![0x08, 0xc3, 0x79, 0xa0];
        d.extend_from_slice(&word_u128(32));
        d.extend_from_slice(&word_u128(s.len() as u128));
        let mut body = s.as_bytes().to_vec();
        while body.len() & 31 != 0 {
            body.push(0);
        }
        d.extend_from_slice(&body);
        bytes_to_hex(&d)
    }

    #[test]
    fn revert_strings_are_decoded_from_node_messages() {
        let msg = format!(
            "server returned an error response: error code 3: execution reverted: 51, data: \"{}\"",
            error_string_payload("51")
        );
        assert_eq!(revert_reason_from_message(&msg), Some("51".to_string()));
        assert_eq!(revert_reason_from_message("execution reverted"), None);
        // truncated or malformed payloads are not guessed at
        assert_eq!(revert_reason_from_message("data: \"0x08c379a0\""), None);
        let mut broken = error_string_payload("hello");
        broken.truncate(broken.len() - 70);
        assert_eq!(revert_reason_from_message(&broken), None);
        // control characters are neutralised
        let nasty = error_string_payload("a\u{1b}[31mb");
        assert_eq!(
            revert_reason_from_message(&nasty),
            Some("a?[31mb".to_string())
        );
    }

    type Seen = Vec<(Option<String>, String, String)>;

    struct Fake {
        reply: Result<String, String>,
        seen: RefCell<Seen>,
    }

    impl Fake {
        fn new(reply: Result<&str, &str>) -> Fake {
            Fake {
                reply: reply.map(String::from).map_err(String::from),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl EthCall for Fake {
        fn eth_call(&self, to: &Address, data: &str) -> Result<String, ChainError> {
            self.seen
                .borrow_mut()
                .push((None, to.to_hex(), data.to_string()));
            self.reply.clone().map_err(ChainError::Transport)
        }
        fn eth_call_from(
            &self,
            from: &Address,
            to: &Address,
            data: &str,
        ) -> Result<String, ChainError> {
            self.seen
                .borrow_mut()
                .push((Some(from.to_hex()), to.to_hex(), data.to_string()));
            self.reply.clone().map_err(ChainError::Transport)
        }
    }

    fn run(reply: Result<&str, &str>) -> (Result<Outcome, ChainError>, Seen) {
        let reader = AaveReader::new(Fake::new(reply), addr(POOL));
        let r = simulate(&reader, Operation::Supply, &addr(ASSET), 5, &addr(ACCT));
        let seen = reader_seen(&reader);
        (r, seen)
    }

    fn reader_seen(r: &AaveReader<Fake>) -> Seen {
        r.transport_for_tests().seen.borrow().clone()
    }

    #[test]
    fn success_is_reported_and_the_call_is_made_from_the_account_to_the_pool() {
        let (r, seen) = run(Ok("0x"));
        assert_eq!(
            r,
            Ok(Outcome::WouldSucceed {
                returned: "0x".into()
            })
        );
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.as_deref(), Some(ACCT));
        assert_eq!(seen[0].1, POOL);
        assert!(seen[0].2.starts_with("0x617ba037"));
    }

    #[test]
    fn a_revert_is_an_answer_not_an_error() {
        let (r, _) = run(Err("execution reverted: ERC20: insufficient allowance"));
        assert_eq!(
            r,
            Ok(Outcome::WouldRevert {
                reason: "execution reverted: ERC20: insufficient allowance".into()
            })
        );
        let msg = format!(
            "error code 3: execution reverted, data: \"{}\"",
            error_string_payload("51")
        );
        let (r, _) = run(Err(&msg));
        assert_eq!(
            r,
            Ok(Outcome::WouldRevert {
                reason: "51".into()
            })
        );
    }

    #[test]
    fn transport_failures_are_errors_never_reverts() {
        let (r, _) = run(Err("connection refused"));
        assert_eq!(r, Err(ChainError::Transport("connection refused".into())));
    }

    #[test]
    fn a_transport_that_cannot_simulate_says_so() {
        struct Plain;
        impl EthCall for Plain {
            fn eth_call(&self, _: &Address, _: &str) -> Result<String, ChainError> {
                Ok("0x".into())
            }
        }
        let reader = AaveReader::new(Plain, addr(POOL));
        let e = simulate(&reader, Operation::Withdraw, &addr(ASSET), 1, &addr(ACCT)).unwrap_err();
        assert!(e.to_string().contains("cannot simulate"), "{e}");
    }
}
