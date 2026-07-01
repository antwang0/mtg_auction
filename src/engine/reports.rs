//! Bug reports / feature requests filed by players, managed by the host, and
//! carried across game resets.

use super::Game;
use crate::model::*;

impl Game {
    /// File a bug report or feature request. `reporter` is the submitter if they
    /// were logged in. Returns the new report id.
    pub fn add_report(&mut self, kind: ReportKind, text: &str, reporter: Option<PlayerId>, now_epoch: u64) -> Result<u64, String> {
        let text = Self::clean_report_text(text)?;
        if self.reports.len() >= 1000 {
            return Err("too many reports on file — ask the host to clear some".into());
        }
        self.report_seq += 1;
        let reporter_name = reporter.map_or_else(|| "anonymous".to_string(), |id| self.name_of(id));
        let id = self.report_seq;
        self.reports.push(Report {
            id,
            kind,
            text,
            reporter,
            reporter_name,
            created: now_epoch,
            resolved: false,
        });
        Ok(id)
    }

    /// Trim and validate report text, returning the cleaned copy.
    fn clean_report_text(text: &str) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("the report is empty".into());
        }
        if text.chars().count() > 2000 {
            return Err("the report is too long (max 2000 characters)".into());
        }
        Ok(text.to_string())
    }

    /// Host: mark a report resolved or reopen it.
    pub fn set_report_resolved(&mut self, id: u64, resolved: bool) -> Result<(), String> {
        let r = self.reports.iter_mut().find(|r| r.id == id).ok_or("no such report")?;
        r.resolved = resolved;
        Ok(())
    }

    /// Host: amend a report's kind and text (to fix a typo or recategorise it).
    pub fn amend_report(&mut self, id: u64, kind: ReportKind, text: &str) -> Result<(), String> {
        let text = Self::clean_report_text(text)?;
        let r = self.reports.iter_mut().find(|r| r.id == id).ok_or("no such report")?;
        r.kind = kind;
        r.text = text;
        Ok(())
    }

    /// Host: delete a report.
    pub fn delete_report(&mut self, id: u64) -> Result<(), String> {
        let before = self.reports.len();
        self.reports.retain(|r| r.id != id);
        if self.reports.len() == before {
            return Err("no such report".into());
        }
        Ok(())
    }

    /// Move the report log out of this game (used to carry it across a reset).
    pub fn take_reports(&mut self) -> (Vec<Report>, u64) {
        (std::mem::take(&mut self.reports), self.report_seq)
    }

    /// Restore a report log (and its id counter) into a freshly set-up game.
    pub fn restore_reports(&mut self, reports: Vec<Report>, seq: u64) {
        self.reports = reports;
        self.report_seq = seq;
    }
}
