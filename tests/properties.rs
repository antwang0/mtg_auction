//! Property-based tests: run random sequences of orders and round closes
//! against the engine and assert the invariants that must always hold —
//! money and cards are conserved, no balance ever breaches the debt limit,
//! and holdings stay non-negative.

use mtg_auction::engine::{Game, Rng};
use mtg_auction::model::{CardId, CardPool, Config, GameMode};
use proptest::prelude::*;
use std::collections::HashMap;

fn config(seed: u64, debt: i64) -> Config {
    Config {
        player_names: vec!["A".into(), "B".into(), "C".into()],
        set: "sample".into(),
        starting_money: 100_000, // $1,000 each
        debt_limit: debt,
        // Effectively never auto-finish either phase during the test.
        primary_rounds: 10_000,
        secondary_rounds: 10_000,
        num_packs: 1,
        pack_size: 8,
        seed,
        primary_round_seconds: 0,
        ..Config::default()
    }
}

#[derive(Debug, Clone)]
struct Op {
    kind: u8,
    player: usize,
    card: usize,
    qty: u32,
    price: i64,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    (0u8..5, 0usize..3, 0usize..32, 0u32..40, 0i64..3000)
        .prop_map(|(kind, player, card, qty, price)| Op { kind, player, card, qty, price })
}

fn total_money(g: &Game) -> i64 {
    g.players.values().map(|p| p.balance).sum()
}
fn total_cards(g: &Game) -> u32 {
    g.players.values().map(|p| p.holdings.values().sum::<u32>()).sum()
}

fn check_invariants(g: &Game, init_money: i64, init_cards: u32, debt: i64) {
    assert_eq!(total_money(g), init_money, "money must be conserved");
    assert_eq!(total_cards(g), init_cards, "card copies must be conserved");
    for p in g.players.values() {
        assert!(p.balance >= -debt, "balance {} breached debt limit -{}", p.balance, debt);
        for &qty in p.holdings.values() {
            assert!(qty > 0, "holdings must only store positive quantities");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(250))]

    #[test]
    fn engine_invariants_hold(
        seed in any::<u64>(),
        debt in 0i64..5000,
        ops in prop::collection::vec(op_strategy(), 0..150),
    ) {
        let mut g = Game::setup(config(seed, debt), CardPool::sample()).unwrap();
        let init_money = total_money(&g);
        let init_cards = total_cards(&g);
        let players = g.player_order.clone();

        for op in ops {
            let player = players[op.player % players.len()];
            let card = g.card_order[op.card % g.card_order.len()];
            // Invalid actions are simply rejected; either way the invariants
            // must hold afterwards.
            match op.kind {
                0 => { let _ = g.place_bid(player, card, op.qty, op.price); }
                1 => { let _ = g.place_offer(player, card, op.qty, op.price); }
                2 => { let _ = g.place_bid(player, card, 0, 0); }   // cancel bid
                3 => { let _ = g.place_offer(player, card, 0, 0); } // cancel offer
                _ => { let _ = g.close_round(); }
            }
            check_invariants(&g, init_money, init_cards, debt);
        }
    }

    /// League closes with arbitrary (even over-balance) bids: bid amendment
    /// must keep every balance non-negative, money moves only between players
    /// and the house (plus the stipend injection), and card copies are
    /// conserved between the house pool and player holdings.
    #[test]
    fn league_close_never_creates_debt_and_conserves_money_and_cards(
        seed in any::<u64>(),
        cycles in prop::collection::vec(
            prop::collection::vec((1u32..4, 0usize..64, 0i64..30_000), 0..60),
            1..4,
        ),
    ) {
        let cfg = Config {
            mode: GameMode::League,
            player_names: vec!["A".into(), "B".into(), "C".into()],
            starting_money: 10_000,
            weekly_stipend: 2_500,
            league_first_auction_day: 20_821, // far-future Sunday
            league_matchmaking_start_day: 20_814,
            seed,
            ..Config::default()
        };
        let mut g = Game::setup(cfg, CardPool::default()).unwrap();
        // Stock the pool with the whole sample set, 1–3 copies each.
        let sample = CardPool::sample();
        let all: Vec<_> = sample.commons.iter()
            .chain(&sample.uncommons)
            .chain(&sample.rares)
            .chain(&sample.mythics)
            .cloned()
            .enumerate()
            .map(|(i, c)| (c, (i % 3 + 1) as u32))
            .collect();
        g.add_cards(CardPool { exact: Some(all), ..CardPool::default() }).unwrap();

        let cards: Vec<CardId> = g.card_order.clone();
        let init_qty: HashMap<CardId, u32> = cards.iter().map(|&c| (c, g.house.held(c))).collect();
        let mut rng = Rng::new(seed);
        let mut stipends = 0i64;

        for bids in cycles {
            if g.open_league_auction(1_000).is_err() {
                break; // the pool sold out entirely
            }
            for (p, ci, price) in bids {
                // Bids above the balance are legal (amended at the close);
                // duplicates on a card replace the earlier bid.
                let _ = g.place_league_bid(p, cards[ci % cards.len()], price);
            }
            g.close_league_auction(&mut rng).unwrap();
            stipends += 3 * 2_500;

            for p in g.players.values() {
                prop_assert!(p.balance >= 0, "player {} went into debt: {}", p.name, p.balance);
            }
            let total = total_money(&g) + g.house.balance;
            prop_assert_eq!(total, 3 * 10_000 + stipends, "money conservation");
            for &c in &cards {
                let held: u32 = g.players.values().map(|p| p.held(c)).sum::<u32>() + g.house.held(c);
                prop_assert_eq!(held, init_qty[&c], "copies of {} conserved", g.cards[&c].name);
            }
        }
    }
}
