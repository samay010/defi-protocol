# 14. Drop Python and Java strategy support

Status: accepted (2026-11-26)

## Context
Strategies could be written in Rust, C, C++, Python and Java. Python and Java need heavy runtimes
(CPython, a JVM) that load a lot of code, reserve large address space, read many files under
`/etc` and `/usr`, and make every sandbox rule harder to keep tight. They also cannot compile to
WebAssembly, so they block the planned WASM/WASI isolation step. The maintainer decided the
project should not carry them.

## Decision
- `Language` is now `rust | c | cpp`. `python`, `py` and `java` are refused in manifests and in
  `protocol create --lang` (usage error, exit 2).
- Templates, build commands and per-language end-to-end tests for Python and Java are removed.
- Existing strategies with `language = "python"` or `"java"` stop loading with
  "unknown language"; they must be rewritten in a supported language.
- The line protocol itself is unchanged and stays language-neutral.

## Consequences
- Smaller attack surface and a smaller sandbox allowlist to review; WASM/WASI becomes applicable
  to every supported language.
- Fewer authors can write strategies. Bringing a language back needs a new ADR with its own
  sandbox and test story.
- Two sandbox tests still shell out to `python3` as a scripting tool (a greedy-memory process and
  a raw `ptrace` call); that is test tooling, not strategy support, and both skip when `python3`
  is absent.
