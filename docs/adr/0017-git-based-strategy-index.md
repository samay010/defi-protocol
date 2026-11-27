# ADR 0017: Git-based strategy index

Status: Accepted

## Context
Users want to discover and share strategies. A hosted marketplace would be a central operator
(against the local-first design, ADR 0002) and would need accounts, moderation and, if paid,
payments. Downloaded strategy code is also the largest new trust risk the project has had.

## Decision
A plain-text index in a git repository (format: `docs/marketplace.md`), read by `protocol market`.
No server, no payments, no token. Free strategies only.

- Parsing lives in `protocol-core` (`market`), std only, strict and bounded; one bad line rejects
  the whole index.
- Fetching shells out to `git` (no new Rust dependency, no HTTP client of our own) with only the
  https transport, no hooks, no submodules, no symlink checkout and no prompts. Index entries must
  be plain public `https://` URLs and a full commit id, so they cannot be read as git options and
  cannot point at local or private hosts.
- Install is all-or-nothing: fetch into a staging directory, delete VCS data and build output
  (not covered by the content hash), refuse symlinks and oversized trees, require the content hash
  to equal the index and `strategy.toml` to match the index, then move into place. Existing
  directories are never overwritten.
- Install never pins and never runs. A downloaded strategy is UNVERIFIED until the user reviews and
  pins it (ADR 0007), and still runs under the usual sandbox or wasm limits.

## Consequences
- The index is a list of claims by whoever runs it. A listed strategy can still be malicious or
  loss-making; the hash only proves you got what the index named.
- Needs `git` and network access; the commands fail clearly without them.
- Signatures, multi-reviewer verification and curation are future work and need their own decision.
