//! Shared vocabulary: actions, execution modes, networks, scenarios, instance status.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionKind {
    Supply,
    Withdraw,
    Repay,
    /// Informational only; never moves funds.
    Alert,
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::Supply => "supply",
            ActionKind::Withdraw => "withdraw",
            ActionKind::Repay => "repay",
            ActionKind::Alert => "alert",
        }
    }

    pub fn parse(s: &str) -> Option<ActionKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "supply" => Some(ActionKind::Supply),
            "withdraw" => Some(ActionKind::Withdraw),
            "repay" => Some(ActionKind::Repay),
            "alert" => Some(ActionKind::Alert),
            _ => None,
        }
    }

    pub fn moves_funds(&self) -> bool {
        !matches!(self, ActionKind::Alert)
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A proposal produced by a strategy (or requested by the user). It is plain data:
/// strategies never sign or send anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub protocol: String,
    pub asset: String,
    /// Micro-units (see `amount`). Zero for alerts.
    pub amount: u64,
    pub note: String,
}

impl Action {
    pub fn alert(protocol: &str, note: &str) -> Action {
        Action {
            kind: ActionKind::Alert,
            protocol: protocol.to_string(),
            asset: String::new(),
            amount: 0,
            note: note.to_string(),
        }
    }

    pub fn new(kind: ActionKind, protocol: &str, asset: &str, amount: u64) -> Action {
        Action {
            kind,
            protocol: protocol.to_string(),
            asset: asset.to_string(),
            amount,
            note: String::new(),
        }
    }

    /// One-line serialization used for pending-approval files.
    pub fn to_line(&self) -> String {
        let clean = |s: &str| s.replace(['|', '\n', '\r'], " ");
        format!(
            "{}|{}|{}|{}|{}",
            self.kind,
            clean(&self.protocol),
            clean(&self.asset),
            self.amount,
            clean(&self.note)
        )
    }

    pub fn from_line(line: &str) -> Result<Action, String> {
        let p: Vec<&str> = line.splitn(5, '|').collect();
        if p.len() != 5 {
            return Err(format!("malformed action line '{line}'"));
        }
        Ok(Action {
            kind: ActionKind::parse(p[0])
                .ok_or_else(|| format!("unknown action kind '{}'", p[0]))?,
            protocol: p[1].to_string(),
            asset: p[2].to_string(),
            amount: p[3].parse().map_err(|_| format!("bad amount '{}'", p[3]))?,
            note: p[4].to_string(),
        })
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == ActionKind::Alert {
            write!(f, "alert [{}] {}", self.protocol, self.note)
        } else {
            write!(
                f,
                "{} {} {} {}",
                self.kind,
                self.protocol,
                self.asset,
                crate::amount::format_amount(self.amount)
            )
        }
    }
}

macro_rules! simple_enum {
    ($name:ident { $($variant:ident => $text:expr),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
            pub fn parse(s: &str) -> Option<$name> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

simple_enum!(Mode {
    Autonomous => "autonomous",
    Approval => "approval",
    Manual => "manual",
});

simple_enum!(Network {
    Simulation => "simulation",
    Testnet => "testnet",
    Mainnet => "mainnet",
});

simple_enum!(Scenario {
    Calm => "calm",
    Stress => "stress",
});

simple_enum!(Status {
    Created => "created",
    Running => "running",
    Paused => "paused",
    Stopped => "stopped",
    Completed => "completed",
    Failed => "failed",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_line_round_trip() {
        let mut a = Action::new(ActionKind::Supply, "aave", "USDC", 5_000_000);
        a.note = "with | pipe\nand newline".into();
        let back = Action::from_line(&a.to_line()).unwrap();
        assert_eq!(back.kind, ActionKind::Supply);
        assert_eq!(back.amount, 5_000_000);
        assert_eq!(back.note, "with   pipe and newline");
    }

    #[test]
    fn enums_parse_case_insensitively() {
        assert_eq!(Mode::parse("Approval"), Some(Mode::Approval));
        assert_eq!(Network::parse("MAINNET"), Some(Network::Mainnet));
        assert_eq!(Status::parse("nope"), None);
        assert_eq!(Scenario::Stress.to_string(), "stress");
    }

    #[test]
    fn bad_action_lines_are_errors() {
        assert!(Action::from_line("supply|aave").is_err());
        assert!(Action::from_line("fly|aave|USDC|1|x").is_err());
        assert!(Action::from_line("supply|aave|USDC|abc|x").is_err());
    }
}
