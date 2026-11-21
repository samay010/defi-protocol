# ADR 0010: `clap` for argument parsing

Status: accepted

## Context
The first release parsed arguments with ~75 lines of hand-written code. Option parsing is a
well-solved problem with many edge cases (quoting, `--opt=value`, abbreviations, help text).

## Decision
Use `clap` 4 (derive). Parsing, help and usage errors come from `clap`. Command handlers are
unchanged; enum-like options (mode, scenario, network, kind) stay strings validated by the
handlers so their messages stay the same. Exit codes are unchanged: 0 success, 1 failure, 2 usage.

## Consequences
- Less hand-written code to audit; help is generated and always matches the options.
- Adds `clap` and its dependency tree (checked by `cargo deny` and `cargo audit` in CI).
- Help and parse-error wording is now `clap`'s.
- `clap` only parses arguments. It never touches funds, strategies or the network.
