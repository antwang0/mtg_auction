//! Tests for the bug-report / feature-request log.

use mtg_auction::engine::Game;
use mtg_auction::model::{CardPool, Config, ReportKind};

fn game() -> Game {
    let cfg = Config {
        player_names: vec!["Alice".into(), "Bob".into()],
        set: "sample".into(),
        ..Config::default()
    };
    Game::setup(cfg, CardPool::sample()).unwrap()
}

#[test]
fn reports_record_kind_text_and_reporter() {
    let mut g = game();
    let id = g.add_report(ReportKind::Bug, "  the timer is wrong  ", Some(1), 100).unwrap();
    let anon = g.add_report(ReportKind::Feature, "dark mode please", None, 200).unwrap();
    assert_ne!(id, anon);

    let bug = g.reports.iter().find(|r| r.id == id).unwrap();
    assert_eq!(bug.kind, ReportKind::Bug);
    assert_eq!(bug.text, "the timer is wrong", "trimmed");
    assert_eq!(bug.reporter, Some(1));
    assert_eq!(bug.reporter_name, "Alice");
    assert!(!bug.resolved);

    let feat = g.reports.iter().find(|r| r.id == anon).unwrap();
    assert_eq!(feat.reporter, None);
    assert_eq!(feat.reporter_name, "anonymous");
}

#[test]
fn empty_or_oversized_reports_are_rejected() {
    let mut g = game();
    assert!(g.add_report(ReportKind::Bug, "   ", None, 0).is_err());
    let long = "x".repeat(2001);
    assert!(g.add_report(ReportKind::Bug, &long, None, 0).is_err());
    assert!(g.reports.is_empty());
}

#[test]
fn host_can_resolve_reopen_and_delete() {
    let mut g = game();
    let id = g.add_report(ReportKind::Bug, "oops", None, 0).unwrap();
    g.set_report_resolved(id, true).unwrap();
    assert!(g.reports[0].resolved);
    g.set_report_resolved(id, false).unwrap();
    assert!(!g.reports[0].resolved);
    g.delete_report(id).unwrap();
    assert!(g.reports.is_empty());
    assert!(g.delete_report(id).is_err(), "already gone");
}

#[test]
fn reports_survive_a_reset() {
    let mut g = game();
    g.add_report(ReportKind::Feature, "keep me", None, 0).unwrap();
    let (reports, seq) = g.take_reports();
    assert!(g.reports.is_empty(), "taken out of the old game");

    // A brand-new game gets the log restored, and ids keep climbing.
    let mut g2 = game();
    g2.restore_reports(reports, seq);
    assert_eq!(g2.reports.len(), 1);
    let next = g2.add_report(ReportKind::Bug, "another", None, 1).unwrap();
    assert_eq!(next, 2, "id counter continued, no collision");
}
