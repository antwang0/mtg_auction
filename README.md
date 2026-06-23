# Draft Auction House

A small Rust web app for a D&D-style draft gamemode driven by a periodic
**call auction**. Open some packs, deal cards and money to the players, then run
N rounds where everyone places bids and offers; at each round close the order
books are matched.

## Running

```bash
cargo run                       # serves http://127.0.0.1:8787
BIND=0.0.0.0:8080 cargo run     # bind elsewhere (use a port >= 1024 unless root)
STATE_FILE=/path/game.json cargo run   # persist to a chosen file
STATE_FILE= cargo run           # disable persistence (in-memory only)
```

The game is **saved to disk** (`game_state.json` by default) on every change and
**reloaded on startup**, so a session — including who's logged in — survives a
restart. Saves are written atomically (temp file + rename), so a crash
mid-write can't corrupt an existing save. The browser receives **live updates** over Server-Sent Events, so all
players see new orders and round closes immediately (a slow poll is kept as a
fallback).

The host fills in the **New Game** form (players, set, starting money, debt
limit, rounds, packs) and hits *Open packs & deal* **on the admin page**
(`/admin`). The first player listed is the host. Setup hands back a **secret
token** for every player; the host shares each one privately.

Players use the main page (`/`). Log in either by clicking the **share link**
the host sends you (`/?t=<token>`, which logs you in and strips the token from
the URL) or by pasting your token into the login box at the top right — your
money, holdings and open orders are then private to you, while everyone's
balances and card counts are public. A **live/offline indicator** in the header
shows whether real-time updates are connected. Submit bids/offers; the host
runs *Close auction & match orders* on the admin page to settle a round. Both
pages poll every 2s so one browser tab per player stays in sync.

The player page has two tabs:

- **Inventory** — your table (holdings, bid/offer tickets with a card-image
  preview, open orders), the player standings and auction results, and a
  **planning table** of every card in the game. Star a card to add it to your
  **want list** (saved in the browser); sort by any column and filter the table.
- **Market** — a **gallery of card tiles** you can sort (name, rarity, type,
  mana value, reference price, last clearing price, supply, your copies) and
  filter.

Both tabs filter by **name, rarity, card type, mana value, and owned/wanted**,
and your filter/sort choices are remembered between visits.

**Click any card** (a tile, a row, or a thumbnail) to open it larger, where you
can **bid or offer right there** — the price pre-fills from the last clearing
price (or reference price), with `+`/`−` nudges and `ref`/`last` shortcuts, and
a live readout of what the bid commits and what you'd have left. The modal also
shows your current order on that card and its recent clearing history.

Other trading conveniences: each card shows your resting **bid/ask inline** and
its **last clearing price**; cards you can't afford are dimmed; your held copies
show how many are **committed to offers**; there's a **cancel-all** button and
an open-orders count; a **toast** summarises your fills when a round closes; and
your balance flashes when a trade settles. After each close, every card records
its **top-of-book spread** (best bid / best offer) so you can see how close you
were even when nothing traded.

### Pages

- **`/`** — the player view (Inventory + Market tabs, as above).
- **`/admin`** — host-only controls: start/reset a game, the player token
  hand-out, closing rounds, the full **order ledger** (every bid and offer) and
  trade history. Logging in here needs the host token (the first player's).

## Sets & card data

Cards (names, rarities and images) come from the [Scryfall](https://scryfall.com)
API. Set the **Scryfall set code** in the New Game form — e.g. `dom`
(Dominaria), `mh3` (Modern Horizons 3), `woe` (Wilds of Eldraine); the full list
is at <https://scryfall.com/sets>. The special code `sample` (the default) uses
a small built-in fantasy set with no images and needs no network — handy for
offline play and tests. Fetched sets are cached in memory per code, so resets
and rematches on the same set are instant.

Each card also carries its **type line**, **mana value** (CMC), **mana cost**,
and a **reference price** — Scryfall's `usd` (TCGplayer market price) — used for
the market sorting/filtering and shown as a sanity check against the auction.
The sample set fabricates plausible types, mana values and by-rarity reference
prices so everything works offline.

## Rules

- **Setup.** `num_packs` packs of `pack_size` cards are opened from the chosen
  set's card pool (one rare-or-better slot, a few uncommons, the rest commons;
  rarities fall back if a set lacks a tier). Every opened card is dealt
  round-robin to the players, and each player starts with `starting_money`.
  Copies of the same card are fungible — a card is a single instrument with one
  order book and players hold a quantity.
- **Money.** All amounts are integer **US cents** on the wire (`1234` = $12.34);
  the UI takes and shows dollars.
- **Orders.** A player may rest at most one **bid** (buy, any card) and one
  **offer** (sell, only cards they hold) per card. Re-submitting replaces the
  previous order; quantity `0` cancels it. A player's bid and offer on the
  **same card may not share the same price**. **Unmatched and partially filled
  orders rest and carry over to the next round** until filled or cancelled.
  Every place/cancel is recorded in the order ledger (visible on the admin page).
- **Debt.** A balance may go as low as `-debt_limit`. When placing bids, a
  player's total resting bid commitment (Σ price × qty) may not exceed
  `balance + debt_limit` — so bids alone can never push you past the allowed
  debt. With `debt_limit = 0`, total bids can't exceed your money at all. Your
  table shows how much you have **committed** to resting bids vs. **available**
  to bid.
- **Round timer.** If `round_seconds > 0`, each round auto-closes that many
  seconds after it opens (a countdown shows in the header); the host can still
  close early. `0` means rounds close only when the host clicks.
- **Matching (per card, at round close).** The highest bid is paired with the
  lowest offer. If they **cross** (bid ≥ offer) they trade at the **mid price**
  `(bid + offer) / 2` (rounded to the nearest cent, half up). This repeats until
  the best remaining bid and offer no longer cross. A player never trades with
  themselves. Because orders persist across rounds, the books are re-validated
  as they fill: a seller never delivers more copies than they currently hold,
  and a buyer is never pushed past their debt limit — so a balance can never
  drop below `-debt_limit` no matter how the books fill.
- **End.** After `rounds` closes the game is finished.
- **Limits.** Order price/quantity and the setup configuration are bounded
  (e.g. price ≤ $1,000,000, ≤ 100k copies, ≤ 100k cards opened) so absurd
  inputs can't overflow the money arithmetic or exhaust memory.

## Auth

Auth is token-based and intentionally simple.

- Setup generates one unguessable token per player (independent of the game
  seed). They're returned to the host to distribute.
- The host hands out one **magic link** per player (`/?t=<token>`, or
  `/admin?t=<token>` for themselves) — opening it logs that player in and clears
  the token from the address bar.
- A request acts as a player by sending its token in the `X-Token` header.
- The first player (the host) is the admin: only their token may **close
  rounds** or **start a new game** over an in-progress one. Players may only
  place orders as themselves.

It's "honor-system among friends with secrets" — tokens are bearer credentials
sent over plain HTTP, so run it on a trusted network, not the open internet.

## Layout

| File | Purpose |
|------|---------|
| `src/model.rs`  | Domain types: `Card`, `Player`, `Order`, `Trade`, `OrderEvent`, `Config`, `Phase`, `CardPool`. Money is `Cents` (i64). |
| `src/engine.rs` | `Game`: pack opening from a `CardPool`, dealing, order validation, the order ledger, token generation, and the matching engine. Includes a seeded xorshift PRNG so deals are reproducible. |
| `src/scryfall.rs` | Fetches a set's cards (names, rarities, images, prices, types, mana values) from the Scryfall API into a `CardPool`, with a per-set in-memory cache. |
| `src/app.rs`    | Shared `App` state: the game behind a mutex, the SSE change-broadcaster, JSON persistence, and the round-timer task. |
| `src/api.rs`    | Axum JSON handlers + token auth + SSE endpoint; `api_router()` wires the `/api/*` routes. |
| `src/main.rs`   | Server bootstrap and routes; serves the embedded player (`/`) and admin (`/admin`) pages. |
| `static/`       | Vanilla HTML/CSS/JS — `index.html`/`app.js` (player), `admin.html`/`admin.js` (host). |
| `tests/matching.rs` | Engine tests: crossing, mid price, price priority, partial fills, debt limits, self-trade, order persistence, stale-offer capping, same-price rule, order ledger, per-round clears, round flow. |
| `tests/api.rs` | HTTP integration tests: setup/state flow, token auth on orders, committed/available funds, same-price rule, admin-only close & ledger, timer auto-close. |
| `tests/persistence.rs` | Save → reload round-trip preserves phase, round, resting orders and tokens. |
| `tests/properties.rs` | Property tests (proptest): random order/close sequences preserve money & card conservation, the debt-limit floor, and non-negative holdings. |

## HTTP API

| Method & path | Auth | Body | Notes |
|---------------|------|------|-------|
| `GET /api/state`  | optional `X-Token` | – | Public state; with a valid token, also that player's private orders, committed/available funds and `am_admin`. Includes the round timer (`round_deadline`, `server_now`). |
| `GET /api/events` | – | – | Server-Sent Events stream; emits on every change so clients refresh live. |
| `POST /api/login` | – | `{token}` | Resolve a token to `{player, name, admin}`; 401 if unknown. |
| `POST /api/setup` | host token (only if a game exists) | `Config` (incl. `set`) | Start a new game; fetches the set from Scryfall and returns each player's token. |
| `POST /api/bid`   | player token | `{player, card, qty, price}` | Place/replace/cancel a bid. |
| `POST /api/offer` | player token | `{player, card, qty, price}` | Place/replace/cancel an offer. |
| `POST /api/close` | host token | – | Match the current round and advance. |
| `GET /api/log`    | host token | – | The full order ledger (every bid/offer place & cancel) plus trade history. |

`GET /api/state` cards include `image`, `ref_price`, `type_line`, `cmc`,
`mana_cost` and `supply` (copies in circulation), used by the market and
planning views. Errors come back as `{ "error": "..." }` with HTTP 400 (bad
input) or 401 (auth). Prices and money are in cents. (The want list is purely
client-side — stored in the browser, never sent to the server.)

## Tests

```bash
cargo test
```
