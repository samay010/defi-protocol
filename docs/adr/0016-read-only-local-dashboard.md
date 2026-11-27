# ADR 0016: Read-only local dashboard

Status: Accepted

## Context
Users want to see instances, status, pending proposals and logs without reading terminal output.
A web page is convenient, but a local web server is also an attack surface: any web page open in
the same browser can try to reach `localhost`, and DNS rebinding can make a hostile site look
like the local one.

## Decision
`protocol dashboard` serves a page from a new `protocol-dashboard` crate (std only, no new
dependencies).

- Binds `127.0.0.1` only; peers that are not loopback are refused.
- Every request needs a random 128-bit token from `/dev/urandom` (printed once, new every run),
  compared in constant time. Without it a random website cannot read anything.
- The `Host` header must be `127.0.0.1:<port>` or `localhost:<port>` (defeats DNS rebinding).
- GET only. The data provider trait has read methods only, so the page cannot approve, reject,
  run, stop or change anything. Those stay in the CLI.
- No CORS headers. CSP `default-src 'none'`, `nosniff`, `no-referrer`, `no-store`, frame denial.
- The page is built with `textContent` only (never `innerHTML`); JSON escapes `< > &`.
- 8 KiB header limit, read timeout, one request at a time, `Connection: close`.

## Consequences
- Plain HTTP on loopback: anyone who can run code as you and see the printed link can use it.
  That is the trust boundary; the page shows no keys because the protocol holds none.
- Unix only (needs `/dev/urandom`).
- Write actions in a UI would need a separate decision (CSRF tokens, confirmation); not done.
