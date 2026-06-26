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
**reloaded on startup**, so a session — including who's logged in, their
passwords, the house inventory and every resting order — survives a restart.
Saves are written atomically (temp file + rename), so a crash mid-write can't
corrupt an existing save. Once an hour a dated snapshot
(`game_state.json.YYYY-MM-DD-HH.bak`) is written alongside it and the most recent
48 are kept — so a save clobbered by a bug (which atomic writes don't guard
against) can be restored by copying a recent backup over it. The browser receives **live updates** over Server-Sent Events, so all
players see new orders and round closes immediately (a slow poll is kept as a
fallback).

### HTTPS / deploying behind a domain

The app itself speaks plain HTTP and is meant to sit behind a reverse proxy that
terminates TLS. A ready-to-edit [`Caddyfile`](Caddyfile) is included; [Caddy](https://caddyserver.com)
obtains and auto-renews a Let's Encrypt certificate for you:

First run the app on loopback, then run Caddy in front of it. The site address
is set by `$SITE_ADDRESS` and defaults to `localhost`:

```bash
BIND=127.0.0.1:8787 cargo run --release            # app on loopback (both cases)

caddy run --config ./Caddyfile                      # local: https://localhost
SITE_ADDRESS=auction.example.com caddy run --config ./Caddyfile   # server: real domain
```

Locally, Caddy serves `https://localhost` with a self-signed cert — run
`caddy trust` once to make your browser accept it. On the server, point the
domain's DNS at the machine and open ports 80 + 443; Caddy then fetches and
auto-renews a real Let's Encrypt cert. The proxy streams the `/api/events` SSE
feed without buffering. nginx works too — just proxy to `127.0.0.1:8787` and
disable buffering on `/api/events`.

The host fills in the **New Game** form on the admin page (`/admin`) — players,
starting money, debt limit, rounds, the **card-pool source** (see below), the
**initial deal per rarity**, and the **house offer pricing** — and hits *Open
packs & deal*. The first player listed is the host. Setup hands back a **secret
token** for every player; the host shares each one privately.

The admin page is also where the host runs the game once it's going: closing
rounds, **adding cards or players mid-game**, offering the **house** inventory
into the auction, the full order ledger, and the ELO ladder (below).

Players use the main page (`/`). Log in any of three ways: click the **share
link** the host sends you (`/?t=<token>`, which logs you in and strips the token
from the URL), paste that **token** into the login box, or — once you've set one
— log in with your **name and password**. Your money, holdings and open orders
are then private to you, while everyone's balances and card counts are public.
A **live/offline indicator** in the header shows whether real-time updates are
connected. Submit bids/offers; the host runs *Close auction & match orders* on
the admin page to settle a round. Both pages poll every 2s so one browser tab
per player stays in sync.

Once logged in you can **set a password** (the *Set password* button in the
header) so you can log in by name from any browser without juggling the token.
⚠️ **Don't reuse a real password** — this site's security is deliberately light
(see [Auth](#auth)), and the password is only lightly protected.

The player page has four tabs:

- **Inventory** — your table (holdings, bid/offer tickets with a card-image
  preview, open orders), the player standings and auction results, your own
  **trade history** (what you've bought and sold, and with whom), and a
  **planning table** of every card in the game. Star a card to add it to your
  **want list** (saved in the browser); sort by any column and filter the table.
- **Market** — a **gallery of card tiles** you can sort (name, rarity, type,
  mana value, reference price, last clearing price, supply, your copies) and
  filter.
- **Ladder** — the post-draft **ELO play ladder**: set how many games you want
  per week and click the times you're free on an availability calendar (shown in
  your local timezone); the server auto-schedules matches against the
  closest-rated, least-recently-met available players. Report your own results
  (your opponent confirms), cancel a match for an ELO penalty, and watch the ELO
  leaderboard.
- **TODO** — a personal **checklist** (set a password, acquire your cards in the
  primary phase, confirm/hand off deliveries, set ladder availability — with a
  badge of how many are outstanding), your **deliveries** (cards to pick up and
  confirm, or to hand off before their 2-day deadline, with a countdown and a
  *Mark received* button), and a **schedule** combining the auction phase/round
  deadline with your upcoming ladder games. The host gets a matching
  **Deliveries** panel on `/admin` showing every delivery with a *Reverse*
  button.

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
  trade history, the **ladder** view (run the scheduler on demand, the ELO
  standings, and a match list with host result overrides), and a **Feedback**
  panel of submitted bug reports / feature requests. Logging in here needs
  the host token (the first player's).

Every page (player and admin) has a **💬 Feedback** button that lets anyone —
logged in or not — file a **bug report or feature request**; they show up in the
host's Feedback panel on `/admin` (where they can be marked done or deleted) and
**survive game resets**.

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

Instead of a set code you can paste a **manual card list** in the New Game form
— one `quantity name` per line, e.g.:

```
3 Lightning Bolt
1 Black Lotus
2 Counterspell
```

A line's leading number (also `3x` form; omitted means `1`) is how many copies
of that card exist, and `#`/`//` lines are treated as comments. You don't have
to type names from memory: the card-list pane has a **"Build the list from a
set"** picker — enter a set code (or `sample`), *Load set*, then filter and add
cards (with a quantity, or *Add all shown ×1*) and it fills in the list for you,
summing duplicates by name. The three pool sources — **sample set**, **Scryfall
set code**, and **pasted card list** — are **mutually exclusive**; pick exactly
one on the New Game form. With a card list,
**exactly those cards** make up the pool (the packs / cards-per-pack settings
don't apply). Card metadata (rarity, image, price, type) is looked up from
Scryfall by name as a best effort, so unknown names — typos or custom cards —
and an unreachable Scryfall both fall back to a plain card, and a manual pool
still works offline.

## Rules

- **Setup.** Cards are opened from the chosen pool source. For a set (sample or
  Scryfall), `num_packs` packs of `pack_size` cards are opened (one rare-or-better
  slot, a few uncommons, the rest commons; rarities fall back if a tier is
  missing); a pasted list is the pool verbatim. Each player starts with
  `starting_money`. Copies of the same card are fungible — a card is a single
  instrument with one order book and players hold a quantity.
- **Dealing.** Each player is dealt up to `deal_commons` / `deal_uncommons` /
  `deal_rares` / `deal_mythics` cards of each rarity (interleaved, so shortages
  fall evenly). Any cards left over go to the **house**. With all four deal
  counts `0`, the legacy behaviour applies: every opened card is dealt
  round-robin and nothing is held back.
- **The house.** Cards opened but not dealt are held by the auction house
  (player id `0`). The host can **list them into the auction** (the *Offer house
  cards* button) priced at each card's **reference price plus Gaussian noise** —
  standard deviation `house_offer_stdev_pct`% of the reference, capped at
  ±`house_offer_cap_pct`%. The house only ever sells; its proceeds accrue to its
  balance, and re-listing re-rolls the prices. Cards without a reference price
  are skipped.
- **Joining / adding cards mid-game.** The host can **add cards** (from a pasted
  list) to the house at any time, and **add a player**, who receives a fresh
  token and their per-rarity allocation drawn from the house.
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
- **Two phases.** The auction runs a **primary** phase then a **secondary**
  phase, each with its own round count and timer. In **primary**, the *bank*
  (house) lists all its unallocated cards into the auction so players acquire
  them; when the primary rounds finish the bank **withdraws its remaining
  offers** (keeping the unsold cards) and the **secondary** phase opens, where
  players trade only with each other. The game finishes after the secondary
  rounds close.
- **Round timer.** Each phase has its own timer, set on the New Game form as a
  number plus a unit (**minutes / hours / days**). If it's `> 0`, each round of
  that phase auto-closes that long after it opens (a countdown shows in the
  header); the host can still close early. `0` means rounds close only when the
  host clicks.
- **Matching (per card, at round close).** The highest bid is paired with the
  lowest offer. If they **cross** (bid ≥ offer) they trade at the **mid price**
  `(bid + offer) / 2` (rounded to the nearest cent, half up). This repeats until
  the best remaining bid and offer no longer cross. A player never trades with
  themselves. Because orders persist across rounds, the books are re-validated
  as they fill: a seller never delivers more copies than they currently hold,
  and a buyer is never pushed past their debt limit — so a balance can never
  drop below `-debt_limit` no matter how the books fill.
- **Delivery & settlement.** Every trade creates a **delivery obligation**: the
  seller must hand the buyer the cards by a **deadline 2 days after the trade**.
  The **buyer marks it received** to settle it. If a (non-bank) seller misses the
  deadline the trade is **automatically reversed** — cards and money are returned
  (best effort: only cards the buyer still holds are reclaimed, and a shortfall is
  flagged for the host) — and the seller pays a **penalty** of
  `delivery_penalty_pct`% of the trade value (rounded up to the next cent) to the
  bank. The **bank never defaults** (buying from the bank is a *retrieve*, not a
  delivery that can fail). The **host sees every delivery and can reverse one** to
  fix an error (no penalty).
- **End.** After the secondary phase's last round closes the game is finished.
- **Limits.** Order price/quantity and the setup configuration are bounded
  (e.g. price ≤ $1,000,000, ≤ 100k copies, ≤ 100k cards opened) so absurd
  inputs can't overflow the money arithmetic or exhaust memory.

## Ladder (ELO play)

Once players have their cards, they play matches against each other on a
self-scheduling **ELO ladder** (the **Ladder** tab). Every player starts at
`starting_elo` (default 1200).

- **Availability & target.** Each player marks the time blocks they're free on a
  calendar and sets how many games they want per week (capped at
  `max_games_per_week`; weeks run **Monday→Sunday**, UTC). There are two blocks a
  day — a **morning and an evening** slot. The host sets their start times on the
  New Game form (entered in the host's local time, stored as fixed UTC hours and
  rendered in each viewer's **local timezone**); availability is bounded to a sane
  number of slots per player.
- **Automatic matchmaking.** Matchmaking **only begins once the primary auction
  phase is over** (so players have their cards first). Scheduling is then
  **event-driven** — it runs the moment someone changes their availability or
  weekly target, or frees a slot by cancelling — plus a periodic pass for the
  passage of time (new days/weeks).
  Each pass pairs available players by **fewest prior meetings, then closest
  ELO** — avoiding rematches where it can and keeping games competitive — and
  never books a player twice in one slot or past their weekly target. The host
  can also trigger a pass on demand.
- **Results.** A player reports their own match; the **opponent confirms** before
  it counts (a counter-report flips who must confirm). Confirming applies the
  standard **ELO update** (K-factor `elo_k`, default 32; win/draw/loss = 1/½/0).
  The host can record any result directly as an override.
- **Cancellation.** A player may cancel a scheduled match, taking a
  `cancel_penalty` ELO hit (default 16); the slot and weekly quota free up.
- **No-shows.** A scheduled match whose slot passes without a confirmed result
  (plus a grace period) is **expired** automatically — no ELO change, and the
  pair becomes eligible to be rescheduled. The host can still record it if it was
  actually played.

The ladder's ELO/scheduling settings (`starting_elo`, `elo_k`, `cancel_penalty`,
`max_games_per_week`, `schedule_window_days`, and `ladder_block_hours` — the two
UTC block start hours) are part of `Config`, have sensible defaults, and are
bounded at setup (e.g. `schedule_window_days ≤ 60`, `max_games_per_week ≤ 50`,
exactly two block hours each `0..=23`) so the auto-scheduler can't be driven into
runaway work.

## Auth

Auth is token-based and intentionally simple.

- Setup generates one short token per player (a 4-hex-char id, kept unique;
  independent of the game seed). They're returned to the host to distribute.
- The host hands out one **magic link** per player (`/?t=<token>`, or
  `/admin?t=<token>` for themselves) — opening it logs that player in and clears
  the token from the address bar.
- A request acts as a player by sending its token in the `X-Token` header.
- A player may **set a password** (while logged in) and then log in by **name +
  password**, which just hands back their token for the session. Passwords are
  stored only as a salted SHA-256 hash — but this is convenience, not real
  security, so the UI warns against reusing a password from anywhere else.
- The first player (the host) is the admin: only their token may **close
  rounds**, **start a new game**, add cards/players, or offer house cards.
  Players may only place orders as themselves.

It's "honor-system among friends with secrets" — short tokens and passwords are
bearer/low-security credentials sent over plain HTTP, so run it on a trusted
network, not the open internet.

## Layout

| File | Purpose |
|------|---------|
| `src/model.rs`  | Domain types: `Card`, `Player` (incl. `elo`), `House`, `Order`, `Trade`, `OrderEvent`, `Config`, `PoolSource`, `Phase`, `CardPool`, and the ladder types (`Ladder`, `Match`, `MatchStatus`, `Standing`). Money is `Cents` (i64). |
| `src/engine.rs` | `Game`: pack opening / per-rarity dealing from a `CardPool`, the house inventory and its noisy offers, mid-game card/player additions, passwords, order validation, the order ledger, short unique token generation, per-player trade history, and the matching engine. Includes a seeded xorshift PRNG (with a Gaussian sampler) so deals are reproducible. |
| `src/ladder.rs` | The ELO ladder on `Game`: availability/target prefs, automatic matchmaking, result reporting (propose/confirm + host override), cancellations, no-show expiry, ELO updates, and standings. |
| `src/scryfall.rs` | Fetches a set's cards (names, rarities, images, prices, types, mana values) from the Scryfall API into a `CardPool`, with a per-set in-memory cache; also parses decklists and batch-looks-up named cards for manual pools. |
| `src/hash.rs`   | Dependency-free SHA-256 (with known-answer tests), used to store salted password hashes. |
| `src/app.rs`    | Shared `App` state: the game behind a mutex, the SSE change-broadcaster, JSON persistence, and the background task (round auto-close + ladder scheduling/expiry). |
| `src/api.rs`    | Axum JSON handlers + token auth + SSE endpoint; `api_router()` wires the `/api/*` routes. |
| `src/main.rs`   | Server bootstrap and routes; serves the embedded player (`/`) and admin (`/admin`) pages. |
| `static/`       | Vanilla HTML/CSS/JS — `util.js` (shared helpers + live-update SSE/poll), `index.html`/`app.js` (player), `admin.html`/`admin.js` (host). |
| `tests/matching.rs` | Engine tests: crossing, mid price, price priority, partial fills, debt limits, self-trade, order persistence, stale-offer capping, same-price rule, order ledger, per-round clears, round flow. |
| `tests/api.rs` | HTTP integration tests: setup/state flow, token auth on orders, committed/available funds, same-price rule, admin-only close & ledger, timer auto-close, and the ladder schedule/report/confirm/cancel flow. |
| `tests/ladder.rs` | Ladder engine tests: availability/target caps, slot scheduling, weekly caps, future-only, rematch avoidance, closest-ELO pairing, propose/confirm + ELO updates, cancellation penalty, no-show expiry, host override, standings, serde round-trip. |
| `tests/house.rs` | Per-rarity dealing + house leftovers, house offers clearing against a bid, the variance cap, mid-game add-cards/add-player from the house, and password name-login. |
| `tests/persistence.rs` | Save → reload round-trip preserves phase, round, resting orders and tokens; hourly backups are dated, idempotent, and pruned. |
| `tests/properties.rs` | Property tests (proptest): random order/close sequences preserve money & card conservation, the debt-limit floor, and non-negative holdings. |

## HTTP API

| Method & path | Auth | Body | Notes |
|---------------|------|------|-------|
| `GET /api/state`  | optional `X-Token` | – | Public state; with a valid token, also that player's private orders, committed/available funds and `am_admin`. Includes the round timer (`round_deadline`, `server_now`). |
| `GET /api/events` | – | – | Server-Sent Events stream; emits on every change so clients refresh live. |
| `POST /api/login` | – | `{token}` | Resolve a token to `{player, name, admin}`; 401 if unknown. |
| `POST /api/password-login` | – | `{name, password}` | Log in by name + password; returns `{player, name, admin, token}` (401 if wrong). |
| `POST /api/set-password` | player token | `{password}` | Set/change your own login password. |
| `POST /api/setup` | host token (only if a game exists) | `Config` (`pool_source` selects `sample`/`scryfall`/`manual`) | Start a new game; opens the chosen pool and returns each player's token. |
| `GET /api/set-cards?set=<code>` | open before a game, host-only once one's running | – | A set's cards (`{name, rarity, ref_price, colors}`, sorted) for the manual-list picker, which can search by name and filter by colour. `colors` is a canonical `WUBRG` string (empty = colorless). `sample` works offline. |
| `POST /api/bid`   | player token | `{player, card, qty, price}` | Place/replace/cancel a bid. |
| `POST /api/offer` | player token | `{player, card, qty, price}` | Place/replace/cancel an offer. |
| `POST /api/close` | host token | – | Match the current round and advance. |
| `POST /api/deliveries/receive` | player token | `{delivery_id}` | The buyer marks one of their deliveries received (settling it). |
| `POST /api/deliveries/reverse` | host token | `{delivery_id}` | Host reverses a delivery to fix an error (no penalty). |
| `POST /api/reports` | optional `X-Token` | `{kind, text}` | File a bug report (`kind: "bug"`) or feature request (`"feature"`). Anyone may submit; a token attributes it to that player. |
| `POST /api/reports/resolve` | host token | `{report_id, resolved}` | Mark a report done or reopen it. |
| `POST /api/reports/delete` | host token | `{report_id}` | Delete a report. |
| `POST /api/cards/add` | host token | `{card_list}` | Add cards (from a list) to the house inventory mid-game. |
| `POST /api/players/add` | host token | `{name}` | Add a player mid-game; deals them from the house and returns `{player, name, token}`. |
| `POST /api/house/offer` | host token | – | List the house's cards into the auction at a noisy reference price; returns `{listed}`. |
| `GET /api/log`    | host token | – | The full order ledger (every bid/offer place & cancel) plus trade history. |
| `GET /api/ladder` | optional `X-Token` | – | ELO standings + all matches + calendar shape (blocks, window, `server_now`); with a token, also that player's availability and weekly target. |
| `POST /api/ladder/availability` | player token | `{slots:[i64]}` | Replace your availability (slot ids). |
| `POST /api/ladder/games` | player token | `{games_per_week}` | Set your weekly game target (≤ `max_games_per_week`). |
| `POST /api/ladder/schedule` | host token | – | Run a scheduling pass now (also expires no-shows). |
| `POST /api/ladder/report` | player token | `{match_id, a_wins, b_wins, draws?}` | Report a result — a player proposes (opponent confirms); the host finalises as an override. |
| `POST /api/ladder/confirm` | player token | `{match_id}` | Confirm the opponent's proposed result, applying ELO. |
| `POST /api/ladder/cancel` | player token | `{match_id}` | Cancel a scheduled match, taking the ELO penalty. |

`GET /api/state` cards include `image`, `ref_price`, `type_line`, `cmc`,
`mana_cost` and `supply` (copies in circulation, including the house), used by
the market and planning views. State also carries the unallocated `house`
inventory and `house_balance`, and for the logged-in player their own
`my_trades` (personal trade history), `my_has_password`, and `my_deliveries` (the
player's pending/settled deliveries; the host also gets `all_deliveries`). Errors come back as
`{ "error": "..." }` with HTTP 400 (bad input) or 401 (auth). Prices and money
are in cents. (The want list is purely client-side — stored in the browser,
never sent to the server.)

## Tests

```bash
cargo test
```
