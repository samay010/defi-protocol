# Coverage-guided fuzzing

Needs nightly Rust and `cargo install cargo-fuzz`. Not part of the normal build.

    cd fuzz
    cargo +nightly fuzz run parse_output -- -max_total_time=300
    cargo +nightly fuzz run manifest     -- -max_total_time=300
    cargo +nightly fuzz run action_line  -- -max_total_time=300
    cargo +nightly fuzz run amount       -- -max_total_time=300

A crash is saved under `fuzz/artifacts/`. Turn it into a regression test in
`crates/core/tests/robustness.rs`. The std-only tests there run on every `cargo test`.
