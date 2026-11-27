//! WebAssembly strategies, run inside the `wasmi` interpreter.
//!
//! A WASM strategy speaks the same line protocol as every other strategy: the tick input is the
//! module's standard input, and what it writes to standard output is parsed by the same strict
//! parser as for processes (`DONE` is mandatory). A fresh instance is created for every tick, so
//! a strategy keeps nothing between ticks except the state the engine hands back to it.
//!
//! Isolation is by construction, not by policy. The host provides only this small subset of
//! `wasi_snapshot_preview1`:
//!
//! | function | behaviour |
//! |---|---|
//! | `fd_read` | fd 0 only (the tick input); every other fd is `EBADF` |
//! | `fd_write` | fd 1 (output, at most 1 MiB) and fd 2 (diagnostics, at most 64 KiB kept); others `EBADF` |
//! | `args_*`, `environ_*` | always empty |
//! | `random_get` | a fixed pseudo-random stream (no entropy; ticks are repeatable) |
//! | `clock_time_get` | `ENOSYS` (a strategy cannot learn the time) |
//! | `fd_prestat_get`, `fd_close`, `sched_yield` | "no preopened directories" / ok |
//! | `proc_exit` | ends the tick with that exit code |
//!
//! There is no file, directory, socket or process function at all. A module that imports anything
//! else fails to instantiate (fail closed). CPU is bounded by fuel (instruction budget) and
//! memory by a store limit; both are per tick.
//!
//! Security notes: this protects the host from the *guest* as far as `wasmi` and this file are
//! correct; neither has been independently audited. The host process itself is not placed in the
//! OS sandbox (there is nothing to place: no child process exists).

use std::path::Path;

use protocol_core::external::{encode_input, parse_output};
use protocol_core::manifest::Manifest;
use protocol_core::strategy::{Strategy, TickInput, TickOutput};
use wasmi::{
    Caller, Config, Engine, Error, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
};

/// Largest `.wasm` file accepted.
pub const MAX_MODULE_BYTES: u64 = 16 * 1024 * 1024;
/// Instructions (fuel) granted per millisecond of the manifest's `tick_timeout_ms`.
///
/// An approximation, deliberately on the low side: the interpreter runs far more than this per
/// millisecond, so a well-behaved strategy never notices the limit, while a runaway loop is
/// stopped after a bounded number of steps whatever the machine's speed.
pub const FUEL_PER_MS: u64 = 20_000;
/// Memory limit when the manifest sets none.
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDOUT: usize = 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const MAX_IOVS: u32 = 1024;
const MAX_CHUNK: usize = 1024 * 1024;

// WASI errno values (wasi_snapshot_preview1).
const ESUCCESS: i32 = 0;
const EBADF: i32 = 8;
const EFAULT: i32 = 21;
const EFBIG: i32 = 22;
const EINVAL: i32 = 28;
const ENOSYS: i32 = 52;

struct Host {
    stdin: Vec<u8>,
    pos: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: Option<i32>,
    rng: u64,
    limits: StoreLimits,
}

fn memory(caller: &Caller<'_, Host>) -> Option<Memory> {
    caller.get_export("memory")?.into_memory()
}

fn read_u32(caller: &Caller<'_, Host>, mem: &Memory, at: usize) -> Option<u32> {
    let mut b = [0u8; 4];
    mem.read(caller, at, &mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn write_u32(caller: &mut Caller<'_, Host>, mem: &Memory, at: usize, v: u32) -> bool {
    mem.write(caller, at, &v.to_le_bytes()).is_ok()
}

/// The `(buf, len)` pair of iovec number `i`, with checked arithmetic.
fn iovec(caller: &Caller<'_, Host>, mem: &Memory, iovs: u32, i: u32) -> Option<(usize, usize)> {
    let at = (iovs as usize).checked_add((i as usize).checked_mul(8)?)?;
    let buf = read_u32(caller, mem, at)? as usize;
    let len = read_u32(caller, mem, at.checked_add(4)?)? as usize;
    Some((buf, len))
}

fn fd_write(mut caller: Caller<'_, Host>, fd: i32, iovs: i32, n: i32, out: i32) -> i32 {
    let Some(mem) = memory(&caller) else {
        return EFAULT;
    };
    if fd != 1 && fd != 2 {
        return EBADF;
    }
    let n = n as u32;
    if n > MAX_IOVS {
        return EINVAL;
    }
    let mut total: usize = 0;
    for i in 0..n {
        let Some((buf, len)) = iovec(&caller, &mem, iovs as u32, i) else {
            return EFAULT;
        };
        let len = len.min(MAX_CHUNK);
        let mut data = vec![0u8; len];
        if mem.read(&caller, buf, &mut data).is_err() {
            return EFAULT;
        }
        let host = caller.data_mut();
        if fd == 1 {
            if host.stdout.len() + data.len() > MAX_STDOUT {
                return EFBIG;
            }
            host.stdout.extend_from_slice(&data);
        } else {
            let room = MAX_STDERR.saturating_sub(host.stderr.len());
            host.stderr.extend_from_slice(&data[..data.len().min(room)]);
        }
        total += data.len();
    }
    if write_u32(&mut caller, &mem, out as u32 as usize, total as u32) {
        ESUCCESS
    } else {
        EFAULT
    }
}

fn fd_read(mut caller: Caller<'_, Host>, fd: i32, iovs: i32, n: i32, out: i32) -> i32 {
    let Some(mem) = memory(&caller) else {
        return EFAULT;
    };
    if fd != 0 {
        return EBADF;
    }
    let n = n as u32;
    if n > MAX_IOVS {
        return EINVAL;
    }
    let mut total: usize = 0;
    for i in 0..n {
        let Some((buf, len)) = iovec(&caller, &mem, iovs as u32, i) else {
            return EFAULT;
        };
        let chunk: Vec<u8> = {
            let h = caller.data();
            let end = h.pos.saturating_add(len.min(MAX_CHUNK)).min(h.stdin.len());
            h.stdin[h.pos..end].to_vec()
        };
        if mem.write(&mut caller, buf, &chunk).is_err() {
            return EFAULT;
        }
        caller.data_mut().pos += chunk.len();
        total += chunk.len();
        if chunk.len() < len {
            break;
        }
    }
    if write_u32(&mut caller, &mem, out as u32 as usize, total as u32) {
        ESUCCESS
    } else {
        EFAULT
    }
}

/// Two zero u32 values: "no arguments / no environment variables".
fn write_two_zeros(mut caller: Caller<'_, Host>, a: i32, b: i32) -> i32 {
    let Some(mem) = memory(&caller) else {
        return EFAULT;
    };
    if write_u32(&mut caller, &mem, a as u32 as usize, 0)
        && write_u32(&mut caller, &mem, b as u32 as usize, 0)
    {
        ESUCCESS
    } else {
        EFAULT
    }
}

fn random_get(mut caller: Caller<'_, Host>, buf: i32, len: i32) -> i32 {
    let Some(mem) = memory(&caller) else {
        return EFAULT;
    };
    let len = (len as u32) as usize;
    if len > MAX_CHUNK {
        return EINVAL;
    }
    let mut data = vec![0u8; len];
    {
        let h = caller.data_mut();
        for b in data.iter_mut() {
            // xorshift64*: repeatable, not secret
            h.rng ^= h.rng >> 12;
            h.rng ^= h.rng << 25;
            h.rng ^= h.rng >> 27;
            *b = (h.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }
    if mem.write(&mut caller, buf as u32 as usize, &data).is_ok() {
        ESUCCESS
    } else {
        EFAULT
    }
}

fn link(linker: &mut Linker<Host>) -> Result<(), String> {
    const M: &str = "wasi_snapshot_preview1";
    let e = |e: wasmi::errors::LinkerError| e.to_string();
    linker.func_wrap(M, "fd_write", fd_write).map_err(e)?;
    linker.func_wrap(M, "fd_read", fd_read).map_err(e)?;
    linker
        .func_wrap(M, "args_sizes_get", write_two_zeros)
        .map_err(e)?;
    linker
        .func_wrap(M, "environ_sizes_get", write_two_zeros)
        .map_err(e)?;
    linker
        .func_wrap(M, "args_get", |_: Caller<'_, Host>, _: i32, _: i32| {
            ESUCCESS
        })
        .map_err(e)?;
    linker
        .func_wrap(M, "environ_get", |_: Caller<'_, Host>, _: i32, _: i32| {
            ESUCCESS
        })
        .map_err(e)?;
    linker.func_wrap(M, "random_get", random_get).map_err(e)?;
    linker
        .func_wrap(
            M,
            "clock_time_get",
            |_: Caller<'_, Host>, _: i32, _: i64, _: i32| ENOSYS,
        )
        .map_err(e)?;
    linker
        .func_wrap(
            M,
            "fd_prestat_get",
            |_: Caller<'_, Host>, _: i32, _: i32| EBADF,
        )
        .map_err(e)?;
    linker
        .func_wrap(M, "fd_close", |_: Caller<'_, Host>, fd: i32| {
            if (0..=2).contains(&fd) {
                ESUCCESS
            } else {
                EBADF
            }
        })
        .map_err(e)?;
    linker
        .func_wrap(M, "sched_yield", |_: Caller<'_, Host>| ESUCCESS)
        .map_err(e)?;
    linker
        .func_wrap(
            M,
            "proc_exit",
            |mut caller: Caller<'_, Host>, code: i32| -> Result<(), Error> {
                caller.data_mut().exit = Some(code);
                Err(Error::i32_exit(code))
            },
        )
        .map_err(e)?;
    Ok(())
}

/// A strategy compiled to WebAssembly (`wasm32-wasip1`).
pub struct WasmStrategy {
    engine: Engine,
    module: Module,
    fuel: u64,
    memory_bytes: usize,
}

impl WasmStrategy {
    /// Load the module the manifest's `entry` names, relative to the strategy directory.
    pub fn from_manifest(m: &Manifest, dir: &Path) -> Result<WasmStrategy, String> {
        let dir = dir
            .canonicalize()
            .map_err(|e| format!("strategy directory {}: {e}", dir.display()))?;
        let path = dir.join(&m.entry).canonicalize().map_err(|e| {
            format!(
                "wasm module '{}': {e} (did you run `protocol build`?)",
                m.entry
            )
        })?;
        // a symlink must not lead the loader outside the strategy directory
        if !path.starts_with(&dir) {
            return Err(format!(
                "wasm module '{}' is outside the strategy directory",
                m.entry
            ));
        }
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !meta.is_file() || meta.len() > MAX_MODULE_BYTES {
            return Err(format!(
                "wasm module '{}' must be a file of at most {MAX_MODULE_BYTES} bytes",
                m.entry
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let memory_bytes = if m.max_memory_mb == 0 {
            DEFAULT_MEMORY_BYTES
        } else {
            usize::try_from(m.max_memory_mb.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX)
        };
        WasmStrategy::from_bytes(&bytes, m.tick_timeout_ms, memory_bytes)
    }

    pub fn from_bytes(
        bytes: &[u8],
        tick_timeout_ms: u64,
        memory_bytes: usize,
    ) -> Result<WasmStrategy, String> {
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg);
        let module =
            Module::new(&engine, bytes).map_err(|e| format!("not a valid wasm module: {e}"))?;
        Ok(WasmStrategy {
            engine,
            module,
            fuel: tick_timeout_ms.saturating_mul(FUEL_PER_MS).max(1),
            memory_bytes,
        })
    }

    /// Run one tick: returns what the module wrote to stdout.
    fn run_once(&self, input: String) -> Result<String, String> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.memory_bytes)
            .instances(1)
            .memories(1)
            .tables(8)
            .table_elements(100_000)
            .build();
        let mut store = Store::new(
            &self.engine,
            Host {
                stdin: input.into_bytes(),
                pos: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit: None,
                rng: 0x9E37_79B9_7F4A_7C15,
                limits,
            },
        );
        store.limiter(|h| &mut h.limits);
        store.set_fuel(self.fuel).map_err(|e| e.to_string())?;
        let mut linker = Linker::<Host>::new(&self.engine);
        link(&mut linker)?;
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| format!("cannot start the wasm module: {e}"))?;
        let start = instance
            .get_typed_func::<(), ()>(&store, "_start")
            .map_err(|e| format!("the wasm module has no usable `_start` export: {e}"))?;
        let result = start.call(&mut store, ());
        let out_of_fuel = store.get_fuel().map(|f| f == 0).unwrap_or(false);
        let host = store.data();
        let stderr = String::from_utf8_lossy(&host.stderr);
        let stderr = stderr.trim();
        let stderr = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.chars().take(200).collect::<String>())
        };
        match (host.exit, result) {
            (Some(0), _) | (None, Ok(())) => {}
            (Some(code), _) => return Err(format!("the strategy exited with code {code}{stderr}")),
            (None, Err(e)) => {
                if out_of_fuel {
                    return Err(
                        "the strategy used up its CPU budget for one tick (tick_timeout_ms)"
                            .to_string(),
                    );
                }
                return Err(format!("the strategy trapped: {e}{stderr}"));
            }
        }
        String::from_utf8(host.stdout.clone())
            .map_err(|_| "the strategy wrote output that is not valid UTF-8".to_string())
    }
}

impl Strategy for WasmStrategy {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
        let out = self.run_once(encode_input(input))?;
        parse_output(&out)
    }
}
