use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use plume_core::Error;

use crate::{GsaAccount, RefreshDevice};

/// A free Apple ID's signing certificate is good for 7 days. Past that the apps
/// it signed are dead anyway, so the install no longer counts for anything.
pub const AUTO_ACCOUNT_WINDOW_DAYS: i64 = 7;

/// How many devices one Apple ID is allowed to carry inside that window before
/// auto-select stops handing it new ones.
pub const AUTO_ACCOUNT_MAX_DEVICES: usize = 3;

/// A successful install: which Apple ID did it, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInstall {
    pub email: String,
    pub installed_at: DateTime<Utc>,
}

/// Value stored per device.
///
/// Configs written before install times were tracked hold a bare email string,
/// so both shapes have to deserialize. Those are dated on load; see
/// [`AccountStore::stamp_undated_entries`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum DeviceAccountEntry {
    Dated(DeviceInstall),
    Undated(String),
}

impl DeviceAccountEntry {
    fn email(&self) -> &str {
        match self {
            Self::Dated(install) => install.email.as_str(),
            Self::Undated(email) => email.as_str(),
        }
    }

    fn installed_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Dated(install) => Some(install.installed_at),
            Self::Undated(_) => None,
        }
    }

    fn is_within(&self, cutoff: DateTime<Utc>) -> bool {
        self.installed_at().is_some_and(|at| at > cutoff)
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct AccountStore {
    selected_account: Option<String>,      // Email
    accounts: HashMap<String, GsaAccount>, // Email -> GsaAccount
    #[serde(default)]
    refreshes: HashMap<String, RefreshDevice>, // UDID -> RefreshDevice (apps?)
    #[serde(default)]
    device_accounts: HashMap<String, DeviceAccountEntry>, // UDID -> who installed, and when
    #[serde(default)]
    auto_select_account: bool, // prefer `device_accounts` over `selected_account` when installing
    #[serde(default)]
    locale: Option<String>, // None = system locale
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl AccountStore {
    pub async fn load(path: &Option<PathBuf>) -> Result<Self, Error> {
        if let Some(path) = path {
            let mut settings = if !path.exists() {
                Self::default()
            } else {
                let contents = tokio::fs::read_to_string(path).await?;
                serde_json::from_str(&contents)?
            };
            settings.path = Some(path.clone());

            if settings.stamp_undated_entries(Utc::now()) {
                settings.save().await?;
            }

            Ok(settings)
        } else {
            Ok(Self::default())
        }
    }

    pub fn load_sync(path: &Option<PathBuf>) -> Result<Self, Error> {
        if let Some(path) = path {
            let mut settings = if !path.exists() {
                Self::default()
            } else {
                let contents = std::fs::read_to_string(path)?;
                serde_json::from_str(&contents)?
            };
            settings.path = Some(path.clone());

            if settings.stamp_undated_entries(Utc::now()) {
                // Best effort: a store we cannot write is still usable this run.
                if let Err(e) = settings.save_sync() {
                    log::warn!("Failed to persist install times for older entries: {e}");
                }
            }

            Ok(settings)
        } else {
            Ok(Self::default())
        }
    }

    pub async fn save(&self) -> Result<(), Error> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            tokio::fs::write(path, serde_json::to_string_pretty(self)?).await?;
        }
        Ok(())
    }

    pub fn save_sync(&self) -> Result<(), Error> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        }
        Ok(())
    }

    pub fn accounts(&self) -> &HashMap<String, GsaAccount> {
        &self.accounts
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.path.clone()
    }

    pub fn get_account(&self, email: &str) -> Option<&GsaAccount> {
        self.accounts.get(email)
    }

    pub async fn accounts_add(&mut self, account: GsaAccount) -> Result<(), Error> {
        let email = self.insert_account(account);
        self.selected_account = Some(email);
        self.save().await
    }

    pub fn accounts_add_sync(&mut self, account: GsaAccount) -> Result<(), Error> {
        let email = self.insert_account(account);
        self.selected_account = Some(email);
        self.save_sync()
    }

    /// Inserts an account, keeping the date it was *first* added.
    ///
    /// Signing in again on an existing email refreshes its tokens; it does not
    /// make the account new. Without this, re-logging in would push an old
    /// account to the front of the "most recently added" order.
    fn insert_account(&mut self, mut account: GsaAccount) -> String {
        let email = account.email().clone();

        if let Some(first_added) = self.accounts.get(&email).and_then(GsaAccount::added_at) {
            account.set_added_at(Some(first_added));
        }

        self.accounts.insert(email.clone(), account);
        email
    }

    pub async fn accounts_remove(&mut self, email: &str) -> Result<(), Error> {
        self.forget_account(email);
        self.save().await
    }

    pub fn accounts_remove_sync(&mut self, email: &str) -> Result<(), Error> {
        self.forget_account(email);
        self.save_sync()
    }

    fn forget_account(&mut self, email: &str) {
        self.accounts.remove(email);
        if self.selected_account.as_deref() == Some(email) {
            self.selected_account = None;
        }
        // Otherwise auto-select would keep pointing devices at an account that is gone.
        self.device_accounts.retain(|_, entry| entry.email() != email);
    }

    pub async fn account_select(&mut self, email: &str) -> Result<(), Error> {
        if self.accounts.contains_key(email) {
            self.selected_account = Some(email.to_string());
            self.save().await
        } else {
            Err(Error::Parse) // we need better errors
        }
    }

    pub fn account_select_sync(&mut self, email: &str) -> Result<(), Error> {
        if self.accounts.contains_key(email) {
            self.selected_account = Some(email.to_string());
            self.save_sync()
        } else {
            Err(Error::Parse) // we need better errors
        }
    }

    pub fn selected_account(&self) -> Option<&GsaAccount> {
        if let Some(email) = &self.selected_account {
            self.accounts.get(email)
        } else {
            None
        }
    }

    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    pub fn set_locale_sync(&mut self, locale: Option<String>) -> Result<(), Error> {
        self.locale = locale;
        self.save_sync()
    }

    pub async fn accounts_add_from_session(
        &mut self,
        email: String,
        account: plume_core::auth::Account,
    ) -> Result<(), Error> {
        let first_name = account.get_name().0;
        let s = plume_core::developer::DeveloperSession::using_account(account).await?;
        let teams_response = s.qh_list_teams().await?;
        let adsid = s.adsid().clone();
        let xcode_gs_token = s.xcode_gs_token().clone();

        let team_id = if teams_response.teams.is_empty() {
            "".to_string()
        } else {
            teams_response.teams[0].team_id.clone()
        };

        let account = GsaAccount::new(email, first_name, adsid, xcode_gs_token, team_id);

        self.accounts_add(account).await?;

        Ok(())
    }

    pub async fn update_account_team(&mut self, email: &str, team_id: String) -> Result<(), Error> {
        if let Some(account) = self.accounts.get_mut(email) {
            account.set_team_id(team_id);
            self.save().await
        } else {
            Err(Error::Parse)
        }
    }

    pub fn update_account_team_sync(&mut self, email: &str, team_id: String) -> Result<(), Error> {
        if let Some(account) = self.accounts.get_mut(email) {
            account.set_team_id(team_id);
            self.save_sync()
        } else {
            Err(Error::Parse)
        }
    }

    pub fn refreshes(&self) -> &HashMap<String, RefreshDevice> {
        &self.refreshes
    }

    pub fn get_refresh_device(&self, udid: &str) -> Option<&RefreshDevice> {
        self.refreshes.get(udid)
    }

    pub async fn add_or_update_refresh_device(
        &mut self,
        device: RefreshDevice,
    ) -> Result<(), Error> {
        self.refreshes.insert(device.udid.clone(), device);
        self.save().await
    }

    pub fn add_or_update_refresh_device_sync(
        &mut self,
        device: RefreshDevice,
    ) -> Result<(), Error> {
        self.refreshes.insert(device.udid.clone(), device);
        self.save_sync()
    }

    pub async fn remove_refresh_device(&mut self, udid: &str) -> Result<(), Error> {
        self.refreshes.remove(udid);
        self.save().await
    }

    pub fn remove_refresh_device_sync(&mut self, udid: &str) -> Result<(), Error> {
        self.refreshes.remove(udid);
        self.save_sync()
    }

    /// Whether installs should prefer the account remembered for the target device.
    pub fn auto_select_account(&self) -> bool {
        self.auto_select_account
    }

    pub fn set_auto_select_account_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.auto_select_account = enabled;
        self.save_sync()
    }

    /// Email of the account that last installed successfully onto `udid`.
    pub fn device_account_email(&self, udid: &str) -> Option<&str> {
        self.device_accounts.get(udid).map(DeviceAccountEntry::email)
    }

    /// When that install happened, if the entry predates install-time tracking.
    pub fn device_installed_at(&self, udid: &str) -> Option<DateTime<Utc>> {
        self.device_accounts
            .get(udid)
            .and_then(DeviceAccountEntry::installed_at)
    }

    /// Records that `email` installed successfully onto `udid`, right now.
    pub fn record_device_account_sync(&mut self, udid: &str, email: &str) -> Result<(), Error> {
        self.record_device_account_at_sync(udid, email, Utc::now())
    }

    /// Same, with an explicit timestamp. Exists so the selection rules can be
    /// tested against a fixed clock.
    pub fn record_device_account_at_sync(
        &mut self,
        udid: &str,
        email: &str,
        installed_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        if udid.is_empty() || !self.accounts.contains_key(email) {
            return Ok(());
        }

        self.device_accounts.insert(
            udid.to_string(),
            DeviceAccountEntry::Dated(DeviceInstall {
                email: email.to_string(),
                installed_at,
            }),
        );
        self.save_sync()
    }

    /// Distinct devices this account has installed onto since `cutoff`.
    ///
    /// Undated entries never count: there is no way to tell whether they fall
    /// inside the window.
    pub fn devices_installed_since(&self, email: &str, cutoff: DateTime<Utc>) -> usize {
        self.device_accounts
            .values()
            .filter(|entry| entry.email() == email && entry.is_within(cutoff))
            .count()
    }

    /// Devices this account is currently carrying inside the 7-day window.
    pub fn devices_in_window(&self, email: &str) -> usize {
        self.devices_installed_since(email, Self::window_cutoff(Utc::now()))
    }

    fn window_cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::days(AUTO_ACCOUNT_WINDOW_DAYS)
    }

    /// Dates entries written before install times were tracked.
    ///
    /// An undated entry still means "this Apple ID installed here". Leaving it
    /// undated would push the device down to the spread-the-load rule and move
    /// it onto some other account, which is the wrong answer for a device that
    /// already has one. Dating it to the moment it is first read keeps it where
    /// it is, and persisting that stamp means it ages out after the usual seven
    /// days rather than being renewed on every launch.
    ///
    /// Returns whether anything changed, so the caller only writes when needed.
    fn stamp_undated_entries(&mut self, now: DateTime<Utc>) -> bool {
        let mut changed = false;

        for entry in self.device_accounts.values_mut() {
            if let DeviceAccountEntry::Undated(email) = entry {
                let email = std::mem::take(email);
                *entry = DeviceAccountEntry::Dated(DeviceInstall {
                    email,
                    installed_at: now,
                });
                changed = true;
            }
        }

        changed
    }

    pub fn forget_device_account_sync(&mut self, udid: &str) -> Result<(), Error> {
        if self.device_accounts.remove(udid).is_none() {
            return Ok(());
        }
        self.save_sync()
    }

    /// The account an install targeting `udid` should use.
    pub fn account_for_device(&self, udid: Option<&str>) -> Option<&GsaAccount> {
        self.auto_account_for_device(udid, Utc::now(), &HashSet::new())
    }

    /// The account an install targeting `udid` should use, with auto-select rules
    /// evaluated against `now` and skipping every email in `refused`.
    ///
    /// `refused` carries the accounts an in-progress run already tried and found
    /// full, so a retry lands on a different one.
    ///
    /// With auto-select on the order is:
    ///
    /// 1. the account that already provisioned this device, while its 7-day
    ///    certificate window is still open, so the device stays on one Apple ID
    ///    instead of burning a slot on another;
    /// 2. otherwise the account carrying the fewest devices in that window, and
    ///    only while it is under the per-account cap, which spreads devices out
    ///    rather than filling one ID and then failing;
    /// 3. otherwise the account ticked in Settings, so auto-select never leaves
    ///    the user with nothing at all.
    ///
    /// With auto-select off it is always the ticked account.
    pub fn auto_account_for_device(
        &self,
        udid: Option<&str>,
        now: DateTime<Utc>,
        refused: &HashSet<String>,
    ) -> Option<&GsaAccount> {
        let not_refused = |account: &&GsaAccount| !refused.contains(account.email());

        if !self.auto_select_account {
            return self.selected_account().filter(not_refused);
        }

        let cutoff = Self::window_cutoff(now);

        // 1. Stay on the Apple ID this device is already signed with.
        if let Some(account) = udid
            .filter(|udid| !udid.is_empty())
            .and_then(|udid| self.device_accounts.get(udid))
            .filter(|entry| entry.is_within(cutoff))
            .and_then(|entry| self.accounts.get(entry.email()))
            .filter(not_refused)
        {
            return Some(account);
        }

        // 2. Spread the load across the accounts that still have room.
        struct Candidate<'a> {
            email: &'a str,
            account: &'a GsaAccount,
            used: usize,
            added_at: Option<DateTime<Utc>>,
        }

        let mut candidates: Vec<Candidate<'_>> = self
            .accounts
            .iter()
            .filter(|(email, _)| !refused.contains(*email))
            .map(|(email, account)| Candidate {
                email: email.as_str(),
                account,
                used: self.devices_installed_since(email, cutoff),
                added_at: account.added_at(),
            })
            .filter(|candidate| candidate.used < AUTO_ACCOUNT_MAX_DEVICES)
            .collect();

        // Least loaded first. Between accounts carrying the same number of
        // devices, prefer the one added most recently: a fresh account has the
        // most life left in it, while older ones are more likely to be near a
        // limit we cannot see from here. `None` sorts below every `Some`, so
        // accounts stored before add dates existed count as the oldest. Email is
        // the last resort so the pick never depends on hash map order.
        candidates.sort_by(|a, b| {
            a.used
                .cmp(&b.used)
                .then_with(|| b.added_at.cmp(&a.added_at))
                .then_with(|| a.email.cmp(b.email))
        });

        if let Some(candidate) = candidates.first() {
            return Some(candidate.account);
        }

        // 3. Everything is full or unknown: use whatever is ticked.
        self.selected_account().filter(not_refused)
    }
}
