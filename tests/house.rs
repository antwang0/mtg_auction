//! Tests for the house inventory: per-rarity dealing, house offers priced off
//! the reference, mid-game card/player additions, and password login.

use mtg_auction::engine::{Game, Rng, HOUSE_ID};
use mtg_auction::model::{CardPool, Config, Rarity};

fn cfg() -> Config {
    Config {
        player_names: vec!["Alice".into(), "Bob".into()],
        set: "sample".into(),
        starting_money: 1_000_000, // plenty to buy from the house
        debt_limit: 0,
        rounds: 5,
        num_packs: 4,
        pack_size: 15,
        seed: 11,
        // Deal a few of each rarity per player; the rest go to the house.
        deal_commons: 2,
        deal_uncommons: 1,
        deal_rares: 1,
        deal_mythics: 0,
        ..Config::default()
    }
}

#[test]
fn per_rarity_dealing_holds_leftovers_in_the_house() {
    let g = Game::setup(cfg(), CardPool::sample()).unwrap();

    // Each player got at most the per-rarity target of each rarity.
    for p in g.players.values() {
        let count = |r: Rarity| p.holdings.iter().filter(|(c, _)| g.cards[c].rarity == r).map(|(_, q)| q).sum::<u32>();
        assert!(count(Rarity::Common) <= 2);
        assert!(count(Rarity::Uncommon) <= 1);
        assert!(count(Rarity::Rare) <= 1);
        assert_eq!(count(Rarity::Mythic), 0, "mythic deal count was 0");
    }

    // Cards opened but not dealt are held by the house, and nothing is lost.
    let player_copies: u32 = g.players.values().map(|p| p.holdings.values().sum::<u32>()).sum();
    let house_copies: u32 = g.house.holdings.values().sum();
    assert!(house_copies > 0, "with small per-rarity deals there should be leftovers");
    assert_eq!(player_copies + house_copies, 4 * 15, "every opened card is accounted for");
}

#[test]
fn house_offers_clear_against_a_high_bid() {
    let mut g = Game::setup(cfg(), CardPool::sample()).unwrap();

    // The house lists its inventory at noisy reference prices.
    let mut rng = Rng::new(999);
    let listed = g.offer_house_cards(&mut rng).unwrap();
    assert!(listed > 0);

    // Pick a card the house is offering and have Alice bid well above any
    // plausible noisy reference price for it.
    let card = *g.house.holdings.keys().next().unwrap();
    let house_qty_before = g.house.held(card);
    let house_offer = g.offers[&(HOUSE_ID, card)].price;
    g.place_bid(1, card, 1, house_offer + 10_000).unwrap();

    let alice_before = g.players[&1].held(card);
    let house_balance_before = g.house.balance;
    let result = g.close_round().unwrap();

    // A trade happened, selling from the house to Alice.
    let trade = result.trades.iter().find(|t| t.card == card).expect("house card traded");
    assert_eq!(trade.seller, HOUSE_ID);
    assert_eq!(trade.seller_name, "House");
    assert_eq!(trade.buyer, 1);
    assert_eq!(g.players[&1].held(card), alice_before + 1);
    assert_eq!(g.house.held(card), house_qty_before - 1);
    assert!(g.house.balance > house_balance_before, "house collected the proceeds");

    // Alice's personal trade history records the buy from the house.
    let hist = g.player_trades(1);
    assert!(hist.iter().any(|(_, t)| t.card == card && t.buyer == 1 && t.seller == HOUSE_ID));
}

#[test]
fn house_offer_price_respects_the_variance_cap() {
    let mut c = cfg();
    c.house_offer_stdev_pct = 1000.0; // huge stdev...
    c.house_offer_cap_pct = 20.0; // ...but capped at ±20% of the reference
    let mut g = Game::setup(c, CardPool::sample()).unwrap();
    let mut rng = Rng::new(1);
    g.offer_house_cards(&mut rng).unwrap();
    for (&(who, card), o) in &g.offers {
        if who != HOUSE_ID {
            continue;
        }
        let ref_price = g.cards[&card].ref_price.unwrap() as f64;
        let lo = (ref_price * 0.8).floor() as i64;
        let hi = (ref_price * 1.2).ceil() as i64;
        assert!(o.price >= lo.max(1) && o.price <= hi, "price {} within ±20% of {}", o.price, ref_price);
    }
}

#[test]
fn add_cards_and_add_player_use_the_house() {
    let mut g = Game::setup(cfg(), CardPool::sample()).unwrap();

    // Add some cards mid-game; they land in the house.
    let before: u32 = g.house.holdings.values().sum();
    let added = g.add_cards(make_manual_pool(&[(3, "Brand New Card")])).unwrap();
    assert_eq!(added, 3);
    let after: u32 = g.house.holdings.values().sum();
    assert_eq!(after, before + 3);
    assert!(g.cards.values().any(|c| c.name == "Brand New Card"), "new card interned into the catalog");

    // Add a late player; they get a token and an allocation drawn from the house.
    let house_before: u32 = g.house.holdings.values().sum();
    let id = g.add_player("Zed".into()).unwrap();
    assert!(g.tokens.contains_key(&id));
    assert_eq!(g.tokens[&id].len(), mtg_auction::engine::TOKEN_LEN);
    let zed_cards: u32 = g.players[&id].holdings.values().sum();
    assert!(zed_cards > 0, "the late player was dealt cards from the house");
    let house_after: u32 = g.house.holdings.values().sum();
    assert_eq!(house_before, house_after + zed_cards, "their cards came out of the house");
}

#[test]
fn passwords_allow_name_login() {
    let mut g = Game::setup(cfg(), CardPool::sample()).unwrap();
    assert!(!g.has_password(1));
    g.set_password(1, "hunter2").unwrap();
    assert!(g.has_password(1));

    assert_eq!(g.player_for_name_password("Alice", "hunter2"), Some(1));
    assert_eq!(g.player_for_name_password("alice", "hunter2"), Some(1), "name match is case-insensitive");
    assert_eq!(g.player_for_name_password("Alice", "wrong"), None);
    assert_eq!(g.player_for_name_password("Nobody", "hunter2"), None);
    assert!(g.set_password(1, "no").is_err(), "rejects a too-short password");
}

// Build a manual `CardPool` from (qty, name) pairs without hitting the network.
fn make_manual_pool(rows: &[(u32, &str)]) -> CardPool {
    use mtg_auction::model::PoolCard;
    let exact = rows
        .iter()
        .map(|(q, n)| {
            (
                PoolCard {
                    name: (*n).to_string(),
                    rarity: Rarity::Common,
                    image: None,
                    ref_price: Some(100),
                    type_line: None,
                    cmc: None,
                    mana_cost: None,
                },
                *q,
            )
        })
        .collect();
    CardPool { set_name: "Custom list".into(), exact: Some(exact), ..Default::default() }
}
