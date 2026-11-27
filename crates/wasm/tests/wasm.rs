//! Behaviour of the WASM runner against small hand-built modules (sources in `fixtures-src/`,
//! built with `clang --target=wasm32 -nostdlib`; they import WASI functions directly, so no
//! WASM toolchain is needed to run these tests).

use std::collections::BTreeMap;

use protocol_core::readonly::{aggregate_snapshot, AggregateView};
use protocol_core::strategy::{Strategy, TickInput};
use protocol_wasm::WasmStrategy;

const ECHO: &[u8] = include_bytes!("fixtures/echo.wasm");
const LOOP: &[u8] = include_bytes!("fixtures/loop.wasm");
const GROW: &[u8] = include_bytes!("fixtures/grow.wasm");
const FD3: &[u8] = include_bytes!("fixtures/fd3.wasm");
const EXIT3: &[u8] = include_bytes!("fixtures/exit3.wasm");
const UNKNOWN: &[u8] = include_bytes!("fixtures/unknown.wasm");
const FLOOD: &[u8] = include_bytes!("fixtures/flood.wasm");

fn tick(s: &mut WasmStrategy, n: u64) -> Result<protocol_core::strategy::TickOutput, String> {
    let snap = aggregate_snapshot(
        n,
        &AggregateView {
            collateral_micro_usd: 3_000_000_000,
            debt_micro_usd: 2_000_000_000,
            liq_threshold_bps: 8500,
        },
    );
    let params = BTreeMap::new();
    let state = BTreeMap::new();
    s.on_tick(&TickInput {
        tick: n,
        snapshot: &snap,
        params: &params,
        state: &state,
    })
}

fn load(bytes: &[u8], timeout_ms: u64, mem: usize) -> WasmStrategy {
    WasmStrategy::from_bytes(bytes, timeout_ms, mem).expect("module loads")
}

#[test]
fn a_module_receives_the_tick_input_and_its_output_is_parsed() {
    let mut s = load(ECHO, 2000, 64 << 20);
    let out = tick(&mut s, 7).unwrap();
    assert!(out.logs.iter().any(|l| l == "got TICK 7"), "{:?}", out.logs);
    assert!(out.logs.iter().any(|l| l == "hello from wasm"));
    assert_eq!(out.state.get("seen").map(String::as_str), Some("1"));
    assert!(out.proposals.is_empty());
}

#[test]
fn ticks_are_repeatable_and_independent() {
    let mut s = load(ECHO, 2000, 64 << 20);
    let a = tick(&mut s, 3).unwrap();
    let b = tick(&mut s, 3).unwrap();
    assert_eq!(a.logs, b.logs);
    let c = tick(&mut s, 4).unwrap();
    assert!(c.logs.iter().any(|l| l == "got TICK 4"));
}

#[test]
fn an_endless_loop_is_stopped_by_the_cpu_budget() {
    let mut s = load(LOOP, 50, 64 << 20);
    let started = std::time::Instant::now();
    let e = tick(&mut s, 1).unwrap_err();
    assert!(e.contains("CPU budget"), "{e}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the budget must stop the loop quickly"
    );
}

#[test]
fn memory_growth_beyond_the_limit_is_refused() {
    // the module asks for ~6.5 GB; a 16 MiB limit must say no (the guest sees -1)
    let mut s = load(GROW, 2000, 16 << 20);
    let out = tick(&mut s, 1).unwrap();
    assert!(
        out.logs.iter().any(|l| l == "grow refused"),
        "{:?}",
        out.logs
    );
}

#[test]
fn only_stdin_stdout_and_stderr_exist() {
    let mut s = load(FD3, 2000, 64 << 20);
    let out = tick(&mut s, 1).unwrap();
    // WASI EBADF is 8
    assert!(
        out.logs.iter().any(|l| l == "fd3 errno 8"),
        "{:?}",
        out.logs
    );
}

#[test]
fn a_nonzero_exit_is_an_error_with_the_diagnostics() {
    let mut s = load(EXIT3, 2000, 64 << 20);
    let e = tick(&mut s, 1).unwrap_err();
    assert!(e.contains("exited with code 3"), "{e}");
    assert!(e.contains("boom on stderr"), "{e}");
}

#[test]
fn a_module_that_imports_file_access_cannot_even_start() {
    let mut s = load(UNKNOWN, 2000, 64 << 20);
    let e = tick(&mut s, 1).unwrap_err();
    assert!(e.contains("cannot start"), "{e}");
}

#[test]
fn output_flooding_is_cut_off() {
    let mut s = load(FLOOD, 2000, 64 << 20);
    assert!(tick(&mut s, 1).is_err());
}

#[test]
fn garbage_is_not_a_module() {
    assert!(WasmStrategy::from_bytes(b"not wasm at all", 2000, 1 << 20).is_err());
    assert!(WasmStrategy::from_bytes(&[], 2000, 1 << 20).is_err());
}
