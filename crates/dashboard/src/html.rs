//! The single page the dashboard serves. Static: no external scripts, fonts or images. All
//! dynamic text is inserted with `textContent`, never `innerHTML`.

pub const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Protocol dashboard</title>
<style>
:root { --bg:#f6f7f9; --fg:#1b1f24; --muted:#5b6672; --card:#fff; --line:#d9dee4; --ok:#1a7f37; --warn:#9a6700; --bad:#cf222e; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#0e1116; --fg:#e6edf3; --muted:#8b97a5; --card:#161b22; --line:#2d333b; --ok:#3fb950; --warn:#d29922; --bad:#f85149; }
}
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.45 system-ui, sans-serif; }
header { padding:12px 16px; border-bottom:1px solid var(--line); display:flex; flex-wrap:wrap; gap:8px 16px; align-items:center; }
h1 { font-size:17px; margin:0; }
.pill { font-size:12px; padding:2px 8px; border-radius:99px; border:1px solid var(--line); color:var(--muted); }
.pill.bad { color:var(--bad); border-color:var(--bad); }
.pill.ok { color:var(--ok); border-color:var(--ok); }
main { padding:12px 16px 40px; max-width:900px; margin:0 auto; }
h2 { font-size:14px; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); margin:22px 0 8px; }
.card { background:var(--card); border:1px solid var(--line); border-radius:8px; padding:10px 12px; margin-bottom:8px; }
.card.click { cursor:pointer; }
.row { display:flex; flex-wrap:wrap; gap:4px 14px; }
.k { color:var(--muted); }
.mono { font-family: ui-monospace, monospace; font-size:13px; }
.err { color:var(--bad); }
pre { margin:0; white-space:pre-wrap; word-break:break-word; font:12.5px/1.4 ui-monospace, monospace; }
button { font:inherit; padding:4px 10px; border:1px solid var(--line); background:var(--card); color:var(--fg); border-radius:6px; }
.note { color:var(--muted); font-size:13px; }
</style>
</head>
<body>
<header>
  <h1>Protocol dashboard</h1>
  <span class="pill ok">read-only</span>
  <span id="kill" class="pill"></span>
  <span id="stamp" class="note"></span>
</header>
<main>
  <div id="fatal" class="card err" hidden></div>
  <div id="detail" hidden></div>
  <h2>Instances</h2>
  <div id="instances"></div>
  <h2>Built-in strategies</h2>
  <div id="strategies"></div>
  <p class="note">This page only shows data. Approving actions, starting, stopping and the kill switch stay in the command line.</p>
</main>
<script>
"use strict";
const token = new URLSearchParams(location.search).get("t") || "";
const $ = (id) => document.getElementById(id);
let selected = null;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}
function kv(parent, k, v, cls) {
  const s = el("span", "");
  s.append(el("span", "k", k + " "), el("span", cls || "", String(v)));
  parent.append(s);
}
async function get(path) {
  const r = await fetch(path + (path.includes("?") ? "&" : "?") + "t=" + encodeURIComponent(token), { cache: "no-store" });
  if (!r.ok) throw new Error(path + " -> HTTP " + r.status);
  return r.json();
}
function fatal(msg) {
  const f = $("fatal");
  f.hidden = !msg;
  f.textContent = msg || "";
}
function statusClass(s) { return s === "running" ? "ok" : (s === "stale" || s === "failed") ? "bad" : ""; }

function renderOverview(o) {
  const kill = $("kill");
  kill.textContent = o.kill_switch ? "kill switch ON" : "kill switch off";
  kill.className = "pill " + (o.kill_switch ? "bad" : "");
  const box = $("instances");
  box.replaceChildren();
  if (!o.instances.length) box.append(el("div", "card note", "No instances yet. Start one with: protocol run <strategy>"));
  for (const i of o.instances) {
    const c = el("div", "card click");
    const top = el("div", "row");
    top.append(el("strong", "", i.name));
    kv(top, "status", i.status, statusClass(i.status));
    kv(top, "mode", i.mode);
    kv(top, "tick", i.tick);
    kv(top, "pending", i.pending);
    c.append(top);
    const sub = el("div", "row note");
    kv(sub, "strategy", i.strategy);
    kv(sub, "network", i.network);
    if (i.last_error) kv(sub, "last error", i.last_error, "err");
    c.append(sub);
    c.addEventListener("click", () => { selected = i.name; refreshDetail(); window.scrollTo(0, 0); });
    box.append(c);
  }
  const sb = $("strategies");
  sb.replaceChildren();
  for (const s of o.strategies) {
    const c = el("div", "card");
    const top = el("div", "row");
    top.append(el("strong", "", s.name));
    kv(top, "version", s.version);
    kv(top, "actions", s.actions.join(", "));
    c.append(top, el("div", "note", s.description));
    sb.append(c);
  }
}

function listCard(parent, title, entries, empty) {
  parent.append(el("h2", "", title));
  const c = el("div", "card");
  if (!entries.length) c.append(el("span", "note", empty));
  for (const [k, v] of entries) {
    const r = el("div", "row");
    r.append(el("span", "mono", k), el("span", "k", v));
    c.append(r);
  }
  parent.append(c);
}

function renderDetail(d) {
  const box = $("detail");
  box.replaceChildren();
  box.hidden = false;
  const head = el("div", "row");
  head.append(el("h2", "", "Instance " + d.name));
  const close = el("button", "", "close");
  close.addEventListener("click", () => { selected = null; box.hidden = true; });
  head.append(close);
  box.append(head);
  const c = el("div", "card");
  const r1 = el("div", "row");
  kv(r1, "status", d.status, statusClass(d.status));
  kv(r1, "mode", d.mode); kv(r1, "network", d.network); kv(r1, "scenario", d.scenario);
  kv(r1, "tick", d.tick); kv(r1, "restarts", d.restarts);
  kv(r1, "total moved (micro)", d.total_moved);
  c.append(r1);
  const r2 = el("div", "row note"); kv(r2, "strategy", d.strategy); c.append(r2);
  if (d.last_error) { const r = el("div", "row"); kv(r, "last error", d.last_error, "err"); c.append(r); }
  box.append(c);
  listCard(box, "Account (micro-units)", d.account.map((a) => [a.kind + " " + a.asset, a.amount]), "No balances recorded yet.");
  listCard(box, "Waiting for approval", d.pending.map((p) => ["#" + p.id + " tick " + p.tick, p.action]), "Nothing waiting.");
  listCard(box, "Strategy state", d.state.map((s) => [s.key, s.value]), "No state.");
  box.append(el("h2", "", "Recent log"));
  const lc = el("div", "card");
  lc.append(el("pre", "", d.log.length ? d.log.join("\n") : "Nothing logged yet."));
  box.append(lc);
}

async function refreshDetail() {
  if (!selected) return;
  try { renderDetail(await get("/api/instance/" + encodeURIComponent(selected))); }
  catch (e) { fatal(String(e)); }
}
async function refresh() {
  try {
    renderOverview(await get("/api/overview"));
    $("stamp").textContent = "updated " + new Date().toLocaleTimeString();
    fatal("");
    await refreshDetail();
  } catch (e) { fatal(String(e) + " (is the dashboard still running? was the link copied completely, including ?t=...?)"); }
}
refresh();
setInterval(refresh, 3000);
</script>
</body>
</html>
"##;
