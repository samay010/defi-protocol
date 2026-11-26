//! Replay of recorded account history, for backtesting.
//!
//! A history is a list of rows, one per tick, each holding the same aggregate numbers a live
//! read-only adapter reports (total collateral, total debt, average liquidation threshold).
//! [`ReplayAdapter`] serves row `tick - 1` as the snapshot for `tick`, so a strategy sees the
//! recorded history exactly as it would have seen it live. Nothing is executed or simulated:
//! like the live read-only adapter it can only show data (alerts are allowed, fund-moving actions
//! are errors), so a backtest records what a strategy WOULD have proposed and what the risk engine
//! WOULD have said, not what would have happened to a position afterwards.
//!
//! CSV format (std only; one header line is required and must be exactly this):
//!
//! ```text
//! label,collateral_usd,debt_usd,liq_threshold_bps
//! 2026-01-01,3000,2000,8500
//! ```
//!
//! `#` lines and blank lines are ignored. Amounts are decimals with at most 6 places; the
//! threshold is whole basis points from 0 to 10000. Labels are free text (at most 64 characters,
//! no commas) shown in reports.

use crate::adapter::{Account, ProtocolAdapter, Snapshot};
use crate::amount::parse_amount;
use crate::readonly::{aggregate_snapshot, AggregateView};
use crate::types::{Action, ActionKind};

/// Largest history accepted, so a bad file cannot exhaust memory or run for hours.
pub const MAX_ROWS: usize = 100_000;
/// Largest CSV text accepted.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const HEADER: &str = "label,collateral_usd,debt_usd,liq_threshold_bps";

const READ_ONLY: &str = "replay data: no action can be executed in a backtest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub view: AggregateView,
}

pub fn parse_csv(text: &str) -> Result<Vec<Row>, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("data file is larger than {MAX_BYTES} bytes"));
    }
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'));
    match lines.next() {
        Some((_, h)) if h == HEADER => {}
        Some((n, h)) => {
            return Err(format!(
                "line {n}: the first line must be exactly `{HEADER}`, found `{h}`"
            ))
        }
        None => return Err("data file is empty".into()),
    }
    let mut rows = Vec::new();
    for (n, line) in lines {
        if rows.len() == MAX_ROWS {
            return Err(format!("more than {MAX_ROWS} rows"));
        }
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        if f.len() != 4 {
            return Err(format!("line {n}: expected 4 comma-separated fields"));
        }
        let label = f[0];
        if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
            return Err(format!(
                "line {n}: label must be 1 to 64 printable characters"
            ));
        }
        let collateral =
            parse_amount(f[1]).map_err(|e| format!("line {n}: collateral_usd: {e}"))?;
        let debt = parse_amount(f[2]).map_err(|e| format!("line {n}: debt_usd: {e}"))?;
        let bps: u32 = f[3].parse().ok().filter(|b| *b <= 10_000).ok_or_else(|| {
            format!("line {n}: liq_threshold_bps must be a whole number 0..=10000")
        })?;
        rows.push(Row {
            label: label.to_string(),
            view: AggregateView {
                collateral_micro_usd: collateral,
                debt_micro_usd: debt,
                liq_threshold_bps: bps,
            },
        });
    }
    if rows.is_empty() {
        return Err("data file has a header but no rows".into());
    }
    Ok(rows)
}

/// What the data itself says, independent of any strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub rows: usize,
    /// Lowest health factor and the (1-based) tick where it first occurs; `None` if there was
    /// never any debt.
    pub min_hf: Option<(f64, u64)>,
    /// Ticks whose health factor is below 1.0 (liquidatable by the protocol's own rule).
    pub unsafe_ticks: u64,
    pub first_unsafe_tick: Option<u64>,
}

pub fn stats(rows: &[Row]) -> Stats {
    let mut s = Stats {
        rows: rows.len(),
        min_hf: None,
        unsafe_ticks: 0,
        first_unsafe_tick: None,
    };
    for (i, r) in rows.iter().enumerate() {
        let tick = i as u64 + 1;
        if let Some(hf) = aggregate_snapshot(tick, &r.view).health_factor() {
            if s.min_hf.is_none_or(|(m, _)| hf < m) {
                s.min_hf = Some((hf, tick));
            }
            if hf < 1.0 {
                s.unsafe_ticks += 1;
                s.first_unsafe_tick.get_or_insert(tick);
            }
        }
    }
    s
}

/// Serves recorded rows as snapshots; tick `t` (1-based) shows row `t - 1`.
pub struct ReplayAdapter {
    id: String,
    rows: Vec<Row>,
    account: Account,
}

impl ReplayAdapter {
    pub fn new(id: &str, rows: Vec<Row>) -> Result<ReplayAdapter, String> {
        let first = rows.first().ok_or("no rows to replay")?;
        let account = first.view.to_account();
        Ok(ReplayAdapter {
            id: id.to_string(),
            rows,
            account,
        })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn label(&self, tick: u64) -> Option<&str> {
        self.row(tick).ok().map(|r| r.label.as_str())
    }

    fn row(&self, tick: u64) -> Result<&Row, String> {
        usize::try_from(tick)
            .ok()
            .and_then(|t| t.checked_sub(1))
            .and_then(|i| self.rows.get(i))
            .ok_or_else(|| format!("tick {tick} is outside the recorded history"))
    }
}

impl ProtocolAdapter for ReplayAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
        Ok(aggregate_snapshot(tick, &self.row(tick)?.view))
    }

    fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String> {
        if action.kind == ActionKind::Alert {
            return self.snapshot(tick);
        }
        Err(READ_ONLY.into())
    }

    fn execute(&mut self, _tick: u64, _action: &Action) -> Result<(), String> {
        Err(READ_ONLY.into())
    }

    fn account(&self) -> &Account {
        &self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::MICRO;

    const GOOD: &str = "# comment\nlabel,collateral_usd,debt_usd,liq_threshold_bps\n\nd1,3000,2000,8500\nd2,2500.5,2000,8500\nd3,2000,2000,8500\n";

    #[test]
    fn parses_a_valid_history() {
        let rows = parse_csv(GOOD).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].label, "d2");
        assert_eq!(rows[1].view.collateral_micro_usd, 2_500_500_000);
        assert_eq!(rows[0].view.liq_threshold_bps, 8500);
    }

    #[test]
    fn rejects_bad_files_with_the_line_number() {
        let h = HEADER;
        for (text, needle) in [
            ("", "empty"),
            (h, "no rows"),
            ("wrong,header\n1,2,3,4", "first line"),
            (&format!("{h}\nd,1,2"), "line 2: expected 4"),
            (&format!("{h}\nd,1,2,3,4"), "expected 4"),
            (&format!("{h}\n,1,2,8000"), "label"),
            (&format!("{h}\nd,abc,2,8000"), "collateral_usd"),
            (&format!("{h}\nd,1,-2,8000"), "debt_usd"),
            (&format!("{h}\nd,1.1234567,2,8000"), "6 decimal"),
            (&format!("{h}\nd,1,2,10001"), "liq_threshold_bps"),
            (&format!("{h}\nd,1,2,-1"), "liq_threshold_bps"),
            (&format!("{h}\nd,1,2,80.5"), "liq_threshold_bps"),
        ] {
            let e = parse_csv(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
        let long = "x".repeat(65);
        assert!(parse_csv(&format!("{h}\n{long},1,2,8000")).is_err());
    }

    #[test]
    fn size_limits_apply() {
        let mut t = String::from(HEADER);
        for _ in 0..=MAX_ROWS {
            t.push_str("\nd,1,1,8000");
        }
        assert!(parse_csv(&t).unwrap_err().contains("more than"));
        let big = "#".repeat(MAX_BYTES + 1);
        assert!(parse_csv(&big).unwrap_err().contains("larger"));
    }

    #[test]
    fn stats_report_the_worst_point_independent_of_any_strategy() {
        let rows = parse_csv(
            "label,collateral_usd,debt_usd,liq_threshold_bps\na,3000,2000,8500\nb,2000,2000,8500\nc,2500,2000,8500\nd,2000,2000,8500\ne,5000,0,8500\n",
        )
        .unwrap();
        let s = stats(&rows);
        assert_eq!(s.rows, 5);
        // hf: 1.275, 0.85, 1.0625, 0.85, none
        let (hf, tick) = s.min_hf.unwrap();
        assert!((hf - 0.85).abs() < 1e-9 && tick == 2, "{hf} {tick}");
        assert_eq!((s.unsafe_ticks, s.first_unsafe_tick), (2, Some(2)));
        let calm = stats(
            &parse_csv("label,collateral_usd,debt_usd,liq_threshold_bps\na,10,0,8000\n").unwrap(),
        );
        assert_eq!(
            (calm.min_hf, calm.unsafe_ticks, calm.first_unsafe_tick),
            (None, 0, None)
        );
    }

    #[test]
    fn the_adapter_serves_row_t_minus_one_at_tick_t() {
        let a = ReplayAdapter::new("aave", parse_csv(GOOD).unwrap()).unwrap();
        assert_eq!(a.id(), "aave");
        let s1 = a.snapshot(1).unwrap();
        assert_eq!(s1.tick, 1);
        assert_eq!(s1.account.supplied["USD"], 3_000 * MICRO);
        let s3 = a.snapshot(3).unwrap();
        assert_eq!(s3.account.supplied["USD"], 2_000 * MICRO);
        assert_eq!(s3.markets["USD"].liq_threshold_bps, 8500);
        assert_eq!(a.label(2), Some("d2"));
        // outside the history is an error, never a wrap-around or a repeat of the last row
        assert!(a.snapshot(0).unwrap_err().contains("outside"));
        assert!(a.snapshot(4).unwrap_err().contains("outside"));
        assert!(a.label(4).is_none());
    }

    #[test]
    fn replay_can_never_move_funds() {
        let mut a = ReplayAdapter::new("aave", parse_csv(GOOD).unwrap()).unwrap();
        for kind in [ActionKind::Supply, ActionKind::Withdraw, ActionKind::Repay] {
            let act = Action::new(kind, "aave", "USD", MICRO);
            assert!(a.simulate(1, &act).unwrap_err().contains("backtest"));
            assert!(a.execute(1, &act).unwrap_err().contains("backtest"));
        }
        let alert = Action::new(ActionKind::Alert, "aave", "USD", 0);
        assert!(a.simulate(1, &alert).is_ok());
    }

    #[test]
    fn an_empty_history_cannot_be_replayed() {
        assert!(ReplayAdapter::new("aave", vec![]).is_err());
    }
}
