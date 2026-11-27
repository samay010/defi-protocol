# Strategy index ("marketplace")

A git-based list of strategies you can search and install with `protocol market`. There is no
server, no account, no payment and no ranking: the index is a text file in a git repository.

## What the index is, and is not
The index only says *where to fetch* a strategy and *exactly what its source must hash to*. It does
not make anything trusted. Every strategy installed from it is UNVERIFIED until you read the source
and run `protocol verify <name> --pin` yourself. Anyone can publish an index; use ones you trust.

## Format
A file named `index.txt` at the top of an https git repository (or any local file). First line
exactly `# protocol-market-index v1`, then one strategy per line, blank lines and `#` comments allowed:

```
name | version | language | sha256 | repo | commit | description
```

| field | rule |
|---|---|
| name | `a-z 0-9 - _`, at most 64, not an official built-in name |
| version | `A-Z a-z 0-9 . - +`, at most 32 |
| language | `rust`, `c`, `cpp` or `wasm` |
| sha256 | 64 lowercase hex: the value `protocol verify` prints for the source |
| repo | plain `https://` git URL on a public host (no credentials, query, IP address or localhost) |
| commit | full 40-character commit id (a branch or tag could move; a commit cannot) |
| description | at most 200 characters |

A bad line rejects the whole index. Limits: 2 MiB, 10,000 entries, no duplicate name+version.

## Commands
```
protocol market search [word] --index <file|https git URL>
protocol market info <name> [--version V] --index ...
protocol market install <name> [--version V] --index ... [--dir custom-strategies]
```
`--index` can be replaced by the `PROTOCOL_MARKET_INDEX` environment variable. `install` needs `git`.

## What install checks (all must pass, else nothing is installed)
1. The destination directory does not exist (nothing is ever overwritten).
2. The repository is fetched over https at the exact commit (hooks, submodules and symlink
   checkout are disabled; git never prompts; 3 minute limit).
3. VCS data (`.git`) and build output (`target`, `build`, ...) are deleted. The content hash does
   not cover them, so a prebuilt binary in a download would otherwise run unreviewed. You build
   from source.
4. Symbolic links, special files, more than 5,000 files, 50 MiB or 20 directory levels: refused.
5. The content hash equals the index entry.
6. `strategy.toml` parses and its name, version and language equal the index entry.

Then it prints the declared permissions and stops. It never pins and never runs anything.

## Publishing (by hand, on purpose)
1. Put the strategy in a git repository and note the full commit id.
2. Get its hash: check out that commit and run `protocol verify <dir>`.
3. Add one line to your index repository's `index.txt` and push.
