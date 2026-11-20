//! Minimal argument handling (std-only). Flags are `--name value` or `--switch`.

pub struct Args {
    items: Vec<String>,
}

impl Args {
    pub fn new(v: &[String]) -> Args {
        Args { items: v.to_vec() }
    }

    /// Remove `--name value` and return the value.
    pub fn take_flag(&mut self, name: &str) -> Result<Option<String>, String> {
        if let Some(i) = self.items.iter().position(|a| a == name) {
            if i + 1 >= self.items.len() {
                return Err(format!("{name} requires a value"));
            }
            let v = self.items.remove(i + 1);
            self.items.remove(i);
            Ok(Some(v))
        } else {
            Ok(None)
        }
    }

    pub fn take_switch(&mut self, name: &str) -> bool {
        if let Some(i) = self.items.iter().position(|a| a == name) {
            self.items.remove(i);
            true
        } else {
            false
        }
    }

    pub fn pop(&mut self) -> Option<String> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.items.remove(0))
        }
    }

    /// Remaining positionals; errors if an unrecognised `--option` is left over.
    pub fn finish(self) -> Result<Vec<String>, String> {
        if let Some(bad) = self.items.iter().find(|a| a.starts_with("--")) {
            return Err(format!("unknown option '{bad}'"));
        }
        Ok(self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Args {
        Args::new(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_switches_and_positionals() {
        let mut x = a(&["--ticks", "5", "name", "--pin", "other"]);
        assert_eq!(x.take_flag("--ticks").unwrap(), Some("5".to_string()));
        assert!(x.take_switch("--pin"));
        assert!(!x.take_switch("--pin"));
        assert_eq!(x.take_flag("--mode").unwrap(), None);
        assert_eq!(x.pop(), Some("name".to_string()));
        assert_eq!(x.finish().unwrap(), vec!["other"]);
    }

    #[test]
    fn missing_value_and_unknown_option() {
        assert!(a(&["--ticks"]).take_flag("--ticks").is_err());
        assert!(a(&["--oops", "x"]).finish().unwrap_err().contains("--oops"));
    }
}
