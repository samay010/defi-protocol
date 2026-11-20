# 0007: Strategy verification by pinned content hash

**Status:** Accepted

**Decision.** A strategy is "verified" for a user only if the exact triple
`(name, version, SHA-256 of source files)` appears in `<home>/verified.list`. The hash covers
file paths and contents, skips build output (`target`, `build`, `__pycache__`, `.git`,
`node_modules`) and does not follow symlinks. Any edit, rename or version bump un-verifies.
Built-in names cannot be shadowed by local directories.

**Consequences.** + Cheap, local, tamper-evident. - Hashing source does not hash the toolchain or
fetched dependencies (a dependency can change under a verified source tree; vendor or lock them
when reviewing). - Pins are per user; shared signed registries and multi-reviewer approval are
future work (see `docs/security.md`). "Verified" means reviewed, never safe or profitable.
