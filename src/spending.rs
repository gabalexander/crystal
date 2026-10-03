//! What background tasks have spent today, by Claude's own count: what the
//! TUI's footer shows as `$4.12 today`, and what `daily_budget_usd` under
//! `[tasks]` in the config is held against. Kept in the database by the
//! day, so a restart doesn't forget it.

use crate::db::Db;
use anyhow::{Result, ensure};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The spending, on a connection to the database of its own: each task's
/// runs add to it from the thread that reads them.
pub struct Spending {
    db: Mutex<Db>,
}

impl Spending {
    pub fn new(db: Db) -> Spending {
        Spending { db: Mutex::new(db) }
    }

    /// What has been spent today. One the database can't say counts as
    /// nothing, and is told about.
    pub fn today(&self) -> f64 {
        self.db
            .lock()
            .unwrap()
            .spent_on(&today())
            .unwrap_or_else(|err| {
                eprintln!("crystal daemon: couldn't read today's spending: {err:#}");
                0.0
            })
    }

    /// Adds `usd` to today's spending.
    pub fn add(&self, usd: f64) {
        if usd <= 0.0 {
            return;
        }
        if let Err(err) = self.db.lock().unwrap().add_spending(&today(), usd) {
            eprintln!("crystal daemon: couldn't write down today's spending: {err:#}");
        }
    }

    /// Refuses a new run once today's spending has reached `budget`, saying
    /// why. A budget of 0 or less is none.
    pub fn check(&self, budget: f64) -> Result<()> {
        if budget <= 0.0 {
            return Ok(());
        }
        let spent = self.today();
        ensure!(
            spent < budget,
            "background tasks have spent ${spent:.2} today, which is past the daily budget of \
             ${budget:.2}: no new run starts until tomorrow, or until `daily_budget_usd` under \
             [tasks] in the config file is raised"
        );
        Ok(())
    }
}

/// Today, on this machine's clock: `2026-10-03`.
fn today() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let now = libc::time_t::try_from(now).unwrap_or_default();
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill in,
    // and both pointers live for the whole call.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut local) };
    format!(
        "{:04}-{:02}-{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spending_in(dir: &tempfile::TempDir) -> Spending {
        Spending::new(Db::open(&dir.path().join("crystal.sock")).unwrap())
    }

    #[test]
    fn spending_adds_up_and_outlives_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let spending = spending_in(&dir);
        assert_eq!(spending.today(), 0.0);
        spending.add(1.25);
        spending.add(0.5);
        spending.add(-1.0);
        assert_eq!(spending.today(), 1.75);
        assert_eq!(spending_in(&dir).today(), 1.75);
    }

    #[test]
    fn another_day_counts_for_nothing_today() {
        let dir = tempfile::tempdir().unwrap();
        let spending = spending_in(&dir);
        spending
            .db
            .lock()
            .unwrap()
            .add_spending("2001-01-01", 9.0)
            .unwrap();
        assert_eq!(spending.today(), 0.0);
    }

    #[test]
    fn a_new_run_is_refused_once_the_budget_is_spent() {
        let dir = tempfile::tempdir().unwrap();
        let spending = spending_in(&dir);
        spending.add(5.0);
        assert!(spending.check(0.0).is_ok(), "no budget");
        assert!(spending.check(6.0).is_ok());
        let err = spending.check(5.0).unwrap_err().to_string();
        assert!(err.contains("$5.00 today"), "{err}");
        assert!(err.contains("daily_budget_usd"), "{err}");
    }
}
