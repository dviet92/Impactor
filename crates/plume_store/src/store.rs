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

/// Default for how many sign-ins one device identity may take within
/// [`IDENTITY_GROUP_WINDOW_HOURS`] when grouping is on.
///
/// Apple watches how many Apple IDs sign in from one device fingerprint in a
/// short span; a machine-wide identity taking dozens of sign-ins in an hour is
/// what gets them locked in bulk. The limit is a rate, not a lifetime total:
/// once a sign-in is older than the window it no longer counts against the
/// identity, and the identity takes new accounts again. Spreading sign-ins over
/// a few identities keeps each under the rate without provisioning a fresh
/// identity per account -- the provisioning round trip with the anisette
/// server is the step that fails most.
/// Configurable: see [`AccountStore::identity_group_size`].
pub const DEFAULT_IDENTITY_GROUP_SIZE: u64 = 5;

/// The range the setting is held to. One identity per Apple ID at the bottom
/// (every sign-in provisions its own, slow but maximally spread out), and a
/// ceiling that stops a typo like 500 from putting every account back on one
/// identity.
pub const MIN_IDENTITY_GROUP_SIZE: u64 = 1;
pub const MAX_IDENTITY_GROUP_SIZE: u64 = 100;

/// The window over which sign-ins count against an identity.
pub const IDENTITY_GROUP_WINDOW_HOURS: i64 = 24;

/// Default pause between sign-ins when adding accounts from a list.
pub const DEFAULT_BULK_GAP_SECS: u64 = 45;

/// Default base wait after Apple answers 429.
pub const DEFAULT_RATE_LIMIT_WAIT_SECS: u64 = 30;

/// Default number of devices a run transfers to at the same time.
///
/// Four keeps a normal USB tree busy without the hub becoming the bottleneck.
/// One turns the parallelism off and restores the old device-after-device run.
pub const DEFAULT_INSTALL_PARALLEL: u64 = 4;

/// How Apple is reached and how patiently: the "Connection" tab in Settings.
///
/// Both waits exist because Apple rate limits sign-ins per network address and
/// device identity. Someone going through a proxy (pool) is not bound by that
/// counter the same way and can turn the waits right down; someone on a plain
/// home connection needs them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkSettings {
    /// Proxy URL for all Apple traffic (`socks5://`, `socks5h://`, `http://`,
    /// `https://`, optional `user:pass@`), or `None` for direct.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Pause between two sign-ins in a bulk add.
    #[serde(default = "default_bulk_gap_secs")]
    pub bulk_gap_secs: u64,
    /// Base wait before retrying after a 429; the second retry waits twice this.
    #[serde(default = "default_rate_limit_wait_secs")]
    pub rate_limit_wait_secs: u64,
    /// How many devices a run transfers to at once.
    #[serde(default = "default_install_parallel")]
    pub install_parallel: u64,
}

fn default_bulk_gap_secs() -> u64 {
    DEFAULT_BULK_GAP_SECS
}

fn default_rate_limit_wait_secs() -> u64 {
    DEFAULT_RATE_LIMIT_WAIT_SECS
}

fn default_install_parallel() -> u64 {
    DEFAULT_INSTALL_PARALLEL
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            proxy_url: None,
            bulk_gap_secs: DEFAULT_BULK_GAP_SECS,
            rate_limit_wait_secs: DEFAULT_RATE_LIMIT_WAIT_SECS,
            install_parallel: DEFAULT_INSTALL_PARALLEL,
        }
    }
}

/// A successful install: which Apple ID did it, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInstall {
    pub email: String,
    pub installed_at: DateTime<Utc>,
}

/// The result of the most recent install attempt on a device.
///
/// Kept so the multi-install picker can colour each device green (last attempt
/// succeeded) or red (it failed), and -- because it is written to disk the
/// moment each device finishes -- so those colours survive the app being
/// closed, or crashing part way through a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInstallStatus {
    pub ok: bool,
    pub at: DateTime<Utc>,
    /// Why the attempt failed, when it did. `None` on success, and on configs
    /// written before failures were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    install_status: HashMap<String, DeviceInstallStatus>, // UDID -> last attempt result
    #[serde(default)]
    auto_select_account: bool, // prefer `device_accounts` over `selected_account` when installing
    #[serde(default)]
    group_identities: bool, // new sign-ins join a shared identity group instead of the machine one
    #[serde(default)]
    identity_group_size: Option<u64>, // sign-ins one identity may take per window; None = default
    #[serde(default)]
    locale: Option<String>, // None = system locale
    #[serde(default)]
    network: NetworkSettings,
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

            if settings.tidy_device_entries(Utc::now()) {
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

            if settings.tidy_device_entries(Utc::now()) {
                // Best effort: a store we cannot write is still usable this run.
                if let Err(e) = settings.save_sync() {
                    log::warn!("Failed to persist tidied device entries: {e}");
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

    /// Inserts an account, replacing any earlier entry for the same address.
    ///
    /// A successful sign-in counts as adding the account afresh: `account`
    /// arrives stamped with the current time and that stamp is kept, so an
    /// Apple ID that is signed in again moves to the front of the
    /// "most recently added" order auto-pick uses. (This used to preserve the
    /// original date; re-adding a list of accounts is how the user asks for
    /// them to be treated as new, so it no longer does.)
    ///
    /// Apple treats the address case-insensitively, so an existing entry that
    /// differs only in case is the same account: it is dropped and the devices
    /// recorded against it are moved to the new spelling, rather than leaving
    /// two entries for one Apple ID.
    fn insert_account(&mut self, mut account: GsaAccount) -> String {
        let email = account.email().clone();

        // Signing in again produces a brand new record, so anything the old one
        // learned and the sign-in cannot know is carried across by hand. The
        // install history is one of those: when the account last signed a
        // device is still true after it signs in again.
        let previous_install = self
            .accounts
            .iter()
            .filter(|(stored, _)| stored.eq_ignore_ascii_case(&email))
            .filter_map(|(_, stored)| stored.last_install_at())
            .max();
        if let Some(at) = previous_install {
            if account.last_install_at().is_none_or(|current| at > current) {
                account.set_last_install_at(Some(at));
            }
        }

        let same_address: Vec<String> = self
            .accounts
            .keys()
            .filter(|stored| **stored != email && stored.eq_ignore_ascii_case(&email))
            .cloned()
            .collect();

        for old in same_address {
            self.accounts.remove(&old);
            for entry in self.device_accounts.values_mut() {
                if entry.email() == old {
                    *entry = DeviceAccountEntry::Dated(DeviceInstall {
                        email: email.clone(),
                        installed_at: entry.installed_at().unwrap_or_else(Utc::now),
                    });
                }
            }
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

    pub fn network(&self) -> &NetworkSettings {
        &self.network
    }

    pub fn set_network_sync(&mut self, network: NetworkSettings) -> Result<(), Error> {
        self.network = network;
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

    /// Whether new sign-ins are spread over shared device identities.
    pub fn group_identities(&self) -> bool {
        self.group_identities
    }

    pub fn set_group_identities_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.group_identities = enabled;
        self.save_sync()
    }

    /// How many sign-ins one identity may take inside the window.
    ///
    /// Unset -- a config written before the setting existed, or a fresh one --
    /// means [`DEFAULT_IDENTITY_GROUP_SIZE`], so behaviour does not change
    /// until it is set. Held to [`MIN_IDENTITY_GROUP_SIZE`]..=[`MAX_IDENTITY_GROUP_SIZE`]
    /// on the way out as well as in, so a config file edited by hand can never
    /// make the placement loop below spin or put every account on one identity. Smaller means sign-ins are spread over more identities, which
    /// is gentler on Apple but pays the provisioning round trip more often --
    /// that round trip is the step that fails most.
    pub fn identity_group_size(&self) -> usize {
        self.identity_group_size
            .unwrap_or(DEFAULT_IDENTITY_GROUP_SIZE)
            .clamp(MIN_IDENTITY_GROUP_SIZE, MAX_IDENTITY_GROUP_SIZE) as usize
    }

    pub fn set_identity_group_size_sync(&mut self, size: u64) -> Result<(), Error> {
        self.identity_group_size =
            Some(size.clamp(MIN_IDENTITY_GROUP_SIZE, MAX_IDENTITY_GROUP_SIZE));
        self.save_sync()
    }

    /// The identity a sign-in for `email` should use, or `None` for the
    /// machine-wide one.
    ///
    /// Decided before the sign-in, because the tokens it produces are bound to
    /// whichever identity made the request.
    ///
    /// An account that already holds a group keeps it, always: signing in
    /// again is the same Apple ID on the same "device", which is what looks
    /// normal to Apple. Only an account with no group is placed -- one never
    /// seen before, one removed and added back, or one stored before grouping
    /// existed. It goes to the lowest group with room, where "room" is a rate:
    /// a group is full while [`AccountStore::identity_group_size`] accounts
    /// signed in under
    /// it within the last [`IDENTITY_GROUP_WINDOW_HOURS`]. Older sign-ins do
    /// not count, so a group that was full yesterday takes accounts again
    /// today, and a new identity is only provisioned when every existing one
    /// is at the rate. With grouping off every sign-in uses the machine
    /// identity, including a re-add of an account that had a group.
    pub fn identity_group_for_sign_in(&self, email: &str) -> Option<u32> {
        self.identity_group_for_sign_in_at(email, Utc::now())
    }

    /// Same, with an explicit "now" so the window can be tested.
    pub fn identity_group_for_sign_in_at(&self, email: &str, now: DateTime<Utc>) -> Option<u32> {
        if !self.group_identities {
            return None;
        }

        if let Some(current) = self
            .accounts
            .iter()
            .find(|(stored, _)| stored.eq_ignore_ascii_case(email))
            .and_then(|(_, account)| account.identity_group())
        {
            return Some(current);
        }

        let size = self.identity_group_size();
        let mut group = 0;
        while self.identity_group_recent_sign_ins(group, now) >= size {
            group += 1;
        }
        Some(group)
    }

    /// The accounts signed in under identity group `group`, whenever that was.
    pub fn identity_group_members(&self, group: u32) -> impl Iterator<Item = &GsaAccount> {
        self.accounts
            .values()
            .filter(move |account| account.identity_group() == Some(group))
    }

    /// How many sign-ins group `group` took inside the window ending at `now`.
    ///
    /// An account's `added_at` is stamped by every successful sign-in (a
    /// re-add re-stamps it), so it is the time of its last sign-in under its
    /// identity. Accounts from before dates were tracked have none and do not
    /// count -- they were signed in long before any window that matters now.
    pub fn identity_group_recent_sign_ins(&self, group: u32, now: DateTime<Utc>) -> usize {
        let cutoff = now - Duration::hours(IDENTITY_GROUP_WINDOW_HOURS);

        self.identity_group_members(group)
            .filter(|account| account.added_at().is_some_and(|at| at > cutoff))
            .count()
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

    /// Marks `email` as carrying its full share of devices from now on.
    ///
    /// For an Apple ID that is unusable for reasons the tool cannot see -- it
    /// signs but the apps will not open, Apple is asking it for verification,
    /// it is simply behaving oddly -- there is nothing in the device records
    /// to make auto-pick skip it. This writes what a full account looks like:
    /// [`AUTO_ACCOUNT_MAX_DEVICES`] entries dated now, under reserved UDIDs
    /// that belong to no real device.
    ///
    /// They age out on their own after [`AUTO_ACCOUNT_WINDOW_DAYS`], exactly
    /// as real installs do, so a mark is a week-long rest rather than a
    /// permanent ban. Marking again restarts that week, and any earlier mark
    /// for the same account is replaced rather than stacked. Real installs are
    /// left alone: the account keeps its history, and clearing the mark brings
    /// its real count back.
    pub fn mark_account_full_sync(&mut self, email: &str) -> Result<(), Error> {
        self.mark_account_full_at_sync(email, Utc::now())
    }

    fn mark_account_full_at_sync(
        &mut self,
        email: &str,
        at: DateTime<Utc>,
    ) -> Result<(), Error> {
        let Some(email) = self.stored_email(email) else {
            return Ok(());
        };

        self.device_accounts
            .retain(|udid, _| !is_manual_full_udid(udid, &email));

        for index in 0..AUTO_ACCOUNT_MAX_DEVICES {
            self.device_accounts.insert(
                manual_full_udid(&email, index),
                DeviceAccountEntry::Dated(DeviceInstall {
                    email: email.clone(),
                    installed_at: at,
                }),
            );
        }

        self.save_sync()
    }

    /// Drops a mark, leaving the account's real installs as they were.
    pub fn clear_account_full_mark_sync(&mut self, email: &str) -> Result<(), Error> {
        let Some(email) = self.stored_email(email) else {
            return Ok(());
        };

        let before = self.device_accounts.len();
        self.device_accounts
            .retain(|udid, _| !is_manual_full_udid(udid, &email));

        if self.device_accounts.len() == before {
            return Ok(());
        }

        self.save_sync()
    }

    /// Whether `email` is carrying a mark that has not aged out.
    pub fn account_is_marked_full(&self, email: &str) -> bool {
        let cutoff = Self::window_cutoff(Utc::now());

        self.stored_email(email).is_some_and(|email| {
            self.device_accounts
                .iter()
                .any(|(udid, entry)| is_manual_full_udid(udid, &email) && entry.is_within(cutoff))
        })
    }

    /// The account list's spelling of `email`, which is the one the device
    /// entries are written under. `None` when no such account is stored.
    fn stored_email(&self, email: &str) -> Option<String> {
        self.accounts
            .keys()
            .find(|stored| stored.eq_ignore_ascii_case(email))
            .cloned()
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

        // The device entry itself is dropped after seven days; this copy on the
        // account is not, so the list can still say when the account was last
        // used. Only ever moves forward: a batch can record devices out of
        // order, and an older one must not rewrite a newer date.
        if let Some(account) = self.accounts.get_mut(email) {
            if account
                .last_install_at()
                .is_none_or(|previous| installed_at > previous)
            {
                account.set_last_install_at(Some(installed_at));
            }
        }

        self.save_sync()
    }

    /// When `email` last signed a device, whenever that was.
    pub fn last_install_at(&self, email: &str) -> Option<DateTime<Utc>> {
        self.accounts
            .get(email)
            .and_then(GsaAccount::last_install_at)
    }

    /// Records how the latest install attempt on `udid` went, and saves at once.
    ///
    /// Called after every device in a run, success or failure, so the picker can
    /// colour it -- and saved immediately rather than at the end of the batch so
    /// a crash mid-run keeps the results of the devices already done.
    /// `error` carries the failure message when `ok` is false. It is dropped on
    /// success, so a device that installs stops showing its previous error.
    pub fn record_install_result_sync(
        &mut self,
        udid: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<(), Error> {
        self.record_install_result_at_sync(udid, ok, error, Utc::now())
    }

    fn record_install_result_at_sync(
        &mut self,
        udid: &str,
        ok: bool,
        error: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<(), Error> {
        if udid.is_empty() {
            return Ok(());
        }

        let error = if ok {
            None
        } else {
            error
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(shorten_install_error)
        };

        self.install_status
            .insert(udid.to_string(), DeviceInstallStatus { ok, at, error });
        self.save_sync()
    }

    /// How the last install attempt on `udid` went: `Some(true)` installed,
    /// `Some(false)` failed, `None` no record (or it has aged out).
    pub fn device_install_ok(&self, udid: &str) -> Option<bool> {
        self.install_status.get(udid).map(|status| status.ok)
    }

    /// Why the last attempt on `udid` failed, when it failed and a reason was
    /// recorded. Stored next to the colour so a red row can still explain
    /// itself after the app is restarted, not only right after the run.
    pub fn device_install_error(&self, udid: &str) -> Option<&str> {
        self.install_status
            .get(udid)
            .filter(|status| !status.ok)
            .and_then(|status| status.error.as_deref())
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

    /// Brings the device records up to date on load: stamps the undated ones,
    /// then drops the ones that have aged out.
    ///
    /// Order matters. An undated entry still means "this Apple ID installed
    /// here"; stamping it first keeps the device on the account it already has
    /// instead of letting the next step delete it outright. Persisting that
    /// stamp is what lets it age out later rather than being renewed on every
    /// launch.
    ///
    /// Anything past the 7-day window is then removed. The certificate that
    /// signed it has expired, so the record can no longer justify reusing that
    /// account, and it was already being ignored everywhere it is read. Deleting
    /// it keeps the file honest and stops it growing forever.
    ///
    /// Returns whether anything changed, so the caller only writes when needed.
    fn tidy_device_entries(&mut self, now: DateTime<Utc>) -> bool {
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

        // Accounts stored before the date was tracked take it from the device
        // entries they still hold. Those are dropped just below, so this has to
        // happen first; it runs once and then finds nothing to do.
        let mut newest: HashMap<String, DateTime<Utc>> = HashMap::new();
        for entry in self.device_accounts.values() {
            if let Some(at) = entry.installed_at() {
                newest
                    .entry(entry.email().to_string())
                    .and_modify(|best| {
                        if at > *best {
                            *best = at;
                        }
                    })
                    .or_insert(at);
            }
        }
        for (email, at) in newest {
            if let Some(account) = self.accounts.get_mut(&email) {
                if account
                    .last_install_at()
                    .is_none_or(|previous| at > previous)
                {
                    account.set_last_install_at(Some(at));
                    changed = true;
                }
            }
        }

        let cutoff = Self::window_cutoff(now);
        let before = self.device_accounts.len();
        self.device_accounts.retain(|_, entry| entry.is_within(cutoff));

        // The install-result colours age out on the same 7-day clock: once the
        // certificate that signed a device has expired, last week's red or green
        // says nothing about today, and dropping it keeps the file from growing.
        let status_before = self.install_status.len();
        self.install_status.retain(|_, status| status.at > cutoff);

        changed
            || self.device_accounts.len() != before
            || self.install_status.len() != status_before
    }

    pub fn forget_device_account_sync(&mut self, udid: &str) -> Result<(), Error> {
        let had_account = self.device_accounts.remove(udid).is_some();
        let had_status = self.install_status.remove(udid).is_some();
        if !had_account && !had_status {
            return Ok(());
        }
        self.save_sync()
    }

    /// The account an install targeting `udid` should use.
    pub fn account_for_device(&self, udid: Option<&str>) -> Option<&GsaAccount> {
        self.auto_account_for_device(udid, Utc::now(), &HashSet::new())
    }

    /// Which account each of `udids` would be signed with, assigned in order.
    ///
    /// [`account_for_device`](Self::account_for_device) answers for one device
    /// in isolation, so asking it about every device hands them all the same
    /// top pick -- which is why the multi-install picker showed one account
    /// under every row. A real run instead assigns them one after another,
    /// counting each device against the account it lands on, so the newest
    /// account fills to its three devices before the next is used.
    ///
    /// This walks `udids` in the given order the same way, on a throwaway copy,
    /// recording each pick as an install so the following device sees the
    /// updated counts. The result maps UDID -> account email for the devices
    /// that got one, and matches what the install will actually do as long as
    /// the caller passes the devices in install order.
    pub fn project_device_accounts(&self, udids: &[String]) -> HashMap<String, String> {
        self.project_device_accounts_at(udids, Utc::now())
    }

    /// The same assignment, grouped: which devices share an Apple ID, in the
    /// order a run reaches them, plus the ones no account had room for.
    ///
    /// Grouping is what lets a run sign once and install the result on several
    /// devices. The provisioning profile Apple hands back lists the team's
    /// devices rather than one of them, so every device registered before the
    /// signature is covered by it -- signing again per device produces a
    /// different file that says the same thing.
    ///
    /// `refused` names accounts this run has already found unusable, so a
    /// re-plan after a failure does not hand the same one back.
    pub fn plan_device_accounts(
        &self,
        udids: &[String],
        refused: &HashSet<String>,
    ) -> (Vec<(String, Vec<String>)>, Vec<String>) {
        self.plan_device_accounts_at(udids, Utc::now(), refused)
    }

    fn plan_device_accounts_at(
        &self,
        udids: &[String],
        now: DateTime<Utc>,
        refused: &HashSet<String>,
    ) -> (Vec<(String, Vec<String>)>, Vec<String>) {
        let mut simulated = self.clone();
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        let mut unassigned = Vec::new();

        for udid in udids {
            let Some(email) = simulated
                .auto_account_for_device(Some(udid), now, refused)
                .map(|account| account.email().clone())
            else {
                unassigned.push(udid.clone());
                continue;
            };

            match groups.iter_mut().find(|(seen, _)| *seen == email) {
                Some((_, devices)) => devices.push(udid.clone()),
                None => groups.push((email.clone(), vec![udid.clone()])),
            }

            // Count it against the account straight away, so the next device
            // sees that account one fuller -- the same bookkeeping the run does
            // when it records each signature.
            simulated.device_accounts.insert(
                udid.clone(),
                DeviceAccountEntry::Dated(DeviceInstall {
                    email,
                    installed_at: now,
                }),
            );
        }

        (groups, unassigned)
    }

    fn project_device_accounts_at(
        &self,
        udids: &[String],
        now: DateTime<Utc>,
    ) -> HashMap<String, String> {
        let mut simulated = self.clone();
        let mut assignment = HashMap::new();

        for udid in udids {
            let Some(email) = simulated
                .auto_account_for_device(Some(udid), now, &HashSet::new())
                .map(|account| account.email().clone())
            else {
                continue;
            };

            assignment.insert(udid.clone(), email.clone());

            // Count this device against the account it landed on, so the next
            // device in the list sees the account one fuller -- exactly what the
            // batch does when it records each successful install.
            simulated.device_accounts.insert(
                udid.clone(),
                DeviceAccountEntry::Dated(DeviceInstall {
                    email,
                    installed_at: now,
                }),
            );
        }

        assignment
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
            .filter(|candidate| candidate.used < AUTO_ACCOUNT_MAX_DEVICES) // room left
            .collect();

        // Newest account first. A freshly added Apple ID is the one to spend:
        // it has the full 7 days of certificate life ahead of it, while older
        // ones are closer to expiring and more likely to be near a limit we
        // cannot see from here. Because the newest account keeps sorting first
        // until it reaches the device cap, it fills to its three devices before
        // the next account is touched -- so adding three fresh accounts uses
        // those three (three devices each) before any older account is reached,
        // which is what the operator expects.
        //
        // Between accounts added at the same moment, the one carrying fewer
        // devices goes first so they fill evenly. `None` sorts below every
        // `Some`, so accounts stored before add dates existed count as oldest.
        // Email is the last resort so the pick never depends on hash map order.
        candidates.sort_by(|a, b| {
            b.added_at
                .cmp(&a.added_at)
                .then_with(|| a.used.cmp(&b.used))
                .then_with(|| a.email.cmp(b.email))
        });

        if let Some(candidate) = candidates.first() {
            return Some(candidate.account);
        }

        // 3. Nothing has room left. The ticked account is only usable here if
        //    it is under the cap too -- and reaching this point means it is
        //    not, because step 2 already considered every account against the
        //    same rule. Handing it out anyway would put a fourth device on an
        //    Apple ID that can carry three, which Apple rejects at signing
        //    time. Returning nothing lets the caller say so before any of that.
        //
        //    A device already signed with an account is not affected: step 1
        //    returns it before we get here, and re-signing a device the account
        //    already carries takes no new slot.
        self.selected_account()
            .filter(not_refused)
            .filter(|account| self.has_device_room(account.email(), cutoff))
    }

    /// Whether `email` can still take a device it does not already carry.
    fn has_device_room(&self, email: &str, cutoff: DateTime<Utc>) -> bool {
        self.devices_installed_since(email, cutoff) < AUTO_ACCOUNT_MAX_DEVICES
    }

    /// How many more devices `email` can take inside the current window.
    ///
    /// The cap minus what it is already carrying, floored at zero. This is what
    /// auto-select has left to give away for that account.
    pub fn remaining_device_slots(&self, email: &str) -> usize {
        AUTO_ACCOUNT_MAX_DEVICES.saturating_sub(self.devices_in_window(email))
    }
}

/// Marks written by [`AccountStore::mark_account_full_sync`] live in the same
/// map as real installs, under UDIDs starting with this. No device reports a
/// UDID with a colon in it, so the two can never be mistaken for each other.
const MANUAL_FULL_PREFIX: &str = "manual-full:";

fn manual_full_udid(email: &str, index: usize) -> String {
    format!("{MANUAL_FULL_PREFIX}{email}:{index}")
}

fn is_manual_full_udid(udid: &str, email: &str) -> bool {
    udid.strip_prefix(MANUAL_FULL_PREFIX)
        .and_then(|rest| rest.rsplit_once(':'))
        .is_some_and(|(stored, _)| stored.eq_ignore_ascii_case(email))
}

/// The longest failure message kept on disk, in characters.
const INSTALL_ERROR_MAX_CHARS: usize = 300;

/// Trims a failure message down to something worth storing and showing.
///
/// Install errors can arrive as a whole wrapped chain, and the picker only has
/// one line to show it in, so anything past the cap is cut on a character
/// boundary and marked with an ellipsis. Newlines collapse to spaces for the
/// same reason.
fn shorten_install_error(message: &str) -> String {
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");

    if flat.chars().count() <= INSTALL_ERROR_MAX_CHARS {
        return flat;
    }

    let mut short: String = flat.chars().take(INSTALL_ERROR_MAX_CHARS).collect();
    short.push('\u{2026}');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate the tests below assume, which is the shipped default.
    const IDENTITY_GROUP_SIZE: usize = DEFAULT_IDENTITY_GROUP_SIZE as usize;

    fn account(email: &str) -> GsaAccount {
        GsaAccount::new(
            email.to_string(),
            "Test".to_string(),
            "adsid".to_string(),
            "token".to_string(),
            "team".to_string(),
        )
    }

    fn account_added_at(email: &str, added_at: DateTime<Utc>) -> GsaAccount {
        let mut account = account(email);
        account.set_added_at(Some(added_at));
        account
    }

    /// A store holding `emails`, with auto-select on and nothing installed yet.
    ///
    /// The first email is also the one ticked in Settings, because that is the
    /// account the last-resort branch reaches for -- a store with nothing
    /// ticked would pass these tests without ever exercising it.
    fn store_with(emails: &[&str]) -> AccountStore {
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        for email in emails {
            store.accounts.insert(email.to_string(), account(email));
        }
        store.selected_account = emails.first().map(|email| email.to_string());
        store
    }

    fn install(store: &mut AccountStore, udid: &str, email: &str, at: DateTime<Utc>) {
        store.device_accounts.insert(
            udid.to_string(),
            DeviceAccountEntry::Dated(DeviceInstall {
                email: email.to_string(),
                installed_at: at,
            }),
        );
    }

    #[test]
    fn a_fresh_account_is_offered_exactly_three_devices() {
        let now = Utc::now();
        let mut store = store_with(&["solo@x.com"]);
        let none = HashSet::new();

        for i in 0..AUTO_ACCOUNT_MAX_DEVICES {
            let udid = format!("udid-{i}");
            let picked = store
                .auto_account_for_device(Some(&udid), now, &none)
                .expect("account should still have room");
            assert_eq!(picked.email(), "solo@x.com");
            install(&mut store, &udid, "solo@x.com", now);
        }

        assert_eq!(store.remaining_device_slots("solo@x.com"), 0);

        // The fourth device gets nothing rather than a fourth slot on the same
        // Apple ID -- this is the case the ticked-account fallback used to let
        // through.
        assert!(
            store
                .auto_account_for_device(Some("udid-3"), now, &none)
                .is_none()
        );
    }

    #[test]
    fn remaining_slots_are_the_cap_minus_what_is_carried() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);

        assert_eq!(store.remaining_device_slots("a@x.com"), 3);

        install(&mut store, "d1", "a@x.com", now);
        assert_eq!(store.remaining_device_slots("a@x.com"), 2);

        install(&mut store, "d2", "a@x.com", now);
        assert_eq!(store.remaining_device_slots("a@x.com"), 1);

        install(&mut store, "d3", "a@x.com", now);
        assert_eq!(store.remaining_device_slots("a@x.com"), 0);
    }

    #[test]
    fn a_device_already_on_a_full_account_still_reinstalls() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);
        let none = HashSet::new();

        for i in 0..AUTO_ACCOUNT_MAX_DEVICES {
            install(&mut store, &format!("d{i}"), "a@x.com", now);
        }

        // Full for anything new, but "d0" is already one of its three: signing
        // it again takes no additional slot.
        assert_eq!(store.remaining_device_slots("a@x.com"), 0);
        assert_eq!(
            store
                .auto_account_for_device(Some("d0"), now, &none)
                .map(|a| a.email().as_str()),
            Some("a@x.com")
        );
    }

    #[test]
    fn a_full_account_hands_the_next_device_to_one_with_room() {
        let now = Utc::now();
        let mut store = store_with(&["full@x.com", "free@x.com"]);
        let none = HashSet::new();

        for i in 0..AUTO_ACCOUNT_MAX_DEVICES {
            install(&mut store, &format!("d{i}"), "full@x.com", now);
        }

        assert_eq!(
            store
                .auto_account_for_device(Some("new-udid"), now, &none)
                .map(|a| a.email().as_str()),
            Some("free@x.com")
        );
    }

    #[test]
    fn slots_come_back_when_installs_age_out() {
        let now = Utc::now();
        let stale = now - Duration::days(AUTO_ACCOUNT_WINDOW_DAYS + 1);
        let mut store = store_with(&["a@x.com"]);

        for i in 0..AUTO_ACCOUNT_MAX_DEVICES {
            install(&mut store, &format!("d{i}"), "a@x.com", stale);
        }

        // The 7-day certificate on those installs has expired, so they no
        // longer occupy the account.
        assert_eq!(store.remaining_device_slots("a@x.com"), 3);
    }

    #[test]
    fn newest_accounts_fill_first_before_older_ones() {
        let now = Utc::now();
        let none = HashSet::new();

        // Three older accounts, none carrying a device, and three added later.
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        store.selected_account = Some("old1@x.com".to_string());

        for (i, email) in ["old1@x.com", "old2@x.com", "old3@x.com"].iter().enumerate() {
            store
                .accounts
                .insert(email.to_string(), account_added_at(email, now - Duration::days(30 + i as i64)));
        }
        for (i, email) in ["new1@x.com", "new2@x.com", "new3@x.com"].iter().enumerate() {
            // new1 newest, new3 oldest of the new set.
            store
                .accounts
                .insert(email.to_string(), account_added_at(email, now - Duration::minutes(i as i64)));
        }

        // Nine installs in a row must land on the three newest accounts, three
        // devices each, and never touch an older account while a newer one has
        // room.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for i in 0..9 {
            let udid = format!("udid-{i}");
            let email = store
                .auto_account_for_device(Some(&udid), now, &none)
                .expect("an account should have room")
                .email()
                .clone();
            assert!(
                email.starts_with("new"),
                "device {i} went to {email}, but a new account still had room"
            );
            install(&mut store, &udid, &email, now);
            *counts.entry(email).or_default() += 1;
        }

        assert_eq!(counts["new1@x.com"], 3);
        assert_eq!(counts["new2@x.com"], 3);
        assert_eq!(counts["new3@x.com"], 3);

        // Only now, with every new account full, does an older one get a device.
        let tenth = store
            .auto_account_for_device(Some("udid-9"), now, &none)
            .expect("older accounts still have room")
            .email()
            .clone();
        assert!(tenth.starts_with("old"), "tenth device should fall to an old account, got {tenth}");
    }

    #[test]
    fn projection_spreads_devices_the_way_a_run_will() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        store.selected_account = Some("new1@x.com".to_string());

        // Two accounts, newest first.
        store
            .accounts
            .insert("new1@x.com".into(), account_added_at("new1@x.com", now));
        store.accounts.insert(
            "new2@x.com".into(),
            account_added_at("new2@x.com", now - Duration::minutes(5)),
        );

        let udids: Vec<String> = (0..5).map(|i| format!("udid-{i}")).collect();
        let projected = store.project_device_accounts_at(&udids, now);

        // The first three fill the newest account, the next two spill to the
        // second -- not all five on one account, which is what asking per device
        // in isolation would have produced.
        assert_eq!(projected["udid-0"], "new1@x.com");
        assert_eq!(projected["udid-1"], "new1@x.com");
        assert_eq!(projected["udid-2"], "new1@x.com");
        assert_eq!(projected["udid-3"], "new2@x.com");
        assert_eq!(projected["udid-4"], "new2@x.com");
    }

    #[test]
    fn install_status_is_recorded_and_ages_out() {
        let now = Utc::now();
        let mut store = AccountStore::default();

        store
            .record_install_result_at_sync("ok-udid", true, None, now)
            .unwrap();
        store
            .record_install_result_at_sync("fail-udid", false, Some("sign in again"), now)
            .unwrap();

        assert_eq!(store.device_install_ok("ok-udid"), Some(true));
        assert_eq!(store.device_install_ok("fail-udid"), Some(false));
        assert_eq!(store.device_install_ok("unknown"), None);
        assert_eq!(store.device_install_error("fail-udid"), Some("sign in again"));
        assert_eq!(store.device_install_error("ok-udid"), None);

        // A newer attempt overwrites the old colour, and clears its error.
        store
            .record_install_result_at_sync("fail-udid", true, None, now)
            .unwrap();
        assert_eq!(store.device_install_ok("fail-udid"), Some(true));
        assert_eq!(store.device_install_error("fail-udid"), None);

        // A status older than the 7-day window is dropped on load-time tidy.
        store
            .record_install_result_at_sync(
                "stale-udid",
                true,
                None,
                now - Duration::days(AUTO_ACCOUNT_WINDOW_DAYS + 1),
            )
            .unwrap();
        store.tidy_device_entries(now);
        assert_eq!(store.device_install_ok("stale-udid"), None);
        assert_eq!(store.device_install_ok("ok-udid"), Some(true));
    }

    #[test]
    fn install_errors_are_flattened_and_capped() {
        let mut store = AccountStore::default();

        store
            .record_install_result_at_sync(
                "wrapped",
                false,
                Some("  install failed:\n    the device refused\r\n"),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(
            store.device_install_error("wrapped"),
            Some("install failed: the device refused")
        );

        // Blank reasons are stored as "no reason", not as an empty line.
        store
            .record_install_result_at_sync("blank", false, Some("   "), Utc::now())
            .unwrap();
        assert_eq!(store.device_install_error("blank"), None);

        let long = "x".repeat(INSTALL_ERROR_MAX_CHARS + 50);
        store
            .record_install_result_at_sync("long", false, Some(&long), Utc::now())
            .unwrap();
        let stored = store.device_install_error("long").unwrap();
        assert_eq!(stored.chars().count(), INSTALL_ERROR_MAX_CHARS + 1);
        assert!(stored.ends_with('\u{2026}'));
    }

    #[test]
    fn marking_an_account_full_uses_up_its_slots_until_it_ages_out() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com", "b@x.com"]);

        assert_eq!(store.remaining_device_slots("a@x.com"), AUTO_ACCOUNT_MAX_DEVICES);
        assert!(!store.account_is_marked_full("a@x.com"));

        store.mark_account_full_sync("a@x.com").unwrap();

        assert!(store.account_is_marked_full("a@x.com"));
        assert_eq!(store.remaining_device_slots("a@x.com"), 0);
        // Auto-pick skips it and reaches for the other account instead.
        assert_eq!(
            store
                .auto_account_for_device(Some("d0"), now, &HashSet::new())
                .map(|account| account.email().as_str()),
            Some("b@x.com")
        );

        // Marking twice does not stack, and the spelling of the email does
        // not have to match the stored one.
        store.mark_account_full_sync("A@X.COM").unwrap();
        assert_eq!(store.devices_in_window("a@x.com"), AUTO_ACCOUNT_MAX_DEVICES);

        // A mark rests the account for a week, then lets it go.
        let stale = now - Duration::days(AUTO_ACCOUNT_WINDOW_DAYS + 1);
        store.mark_account_full_at_sync("a@x.com", stale).unwrap();
        assert!(!store.account_is_marked_full("a@x.com"));
        store.tidy_device_entries(now);
        assert_eq!(store.remaining_device_slots("a@x.com"), AUTO_ACCOUNT_MAX_DEVICES);
    }

    #[test]
    fn a_mark_can_be_cleared_without_touching_real_installs() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);
        install(&mut store, "d0", "a@x.com", now);

        store.mark_account_full_sync("a@x.com").unwrap();
        assert_eq!(store.remaining_device_slots("a@x.com"), 0);

        store.clear_account_full_mark_sync("a@x.com").unwrap();

        assert!(!store.account_is_marked_full("a@x.com"));
        assert_eq!(store.device_account_email("d0"), Some("a@x.com"));
        assert_eq!(
            store.remaining_device_slots("a@x.com"),
            AUTO_ACCOUNT_MAX_DEVICES - 1
        );
    }

    #[test]
    fn a_mark_belongs_to_one_account_only() {
        assert!(is_manual_full_udid("manual-full:a@x.com:0", "a@x.com"));
        assert!(is_manual_full_udid("manual-full:a@x.com:2", "A@X.COM"));
        assert!(!is_manual_full_udid("manual-full:ab@x.com:0", "a@x.com"));
        assert!(!is_manual_full_udid("00008030-001A2B3C4D5E6F70", "a@x.com"));
    }

    #[test]
    fn removed_account_is_forgotten() {
        let mut store = store_with(&["a@x.com", "b@x.com"]);
        install(&mut store, "d0", "a@x.com", Utc::now());

        store.forget_account("a@x.com");

        assert!(store.get_account("a@x.com").is_none());
        // The device it installed onto is no longer pointed at the gone account.
        assert!(store.device_account_email("d0").is_none());
    }

    #[test]
    fn re_adding_an_account_makes_it_new_again() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.auto_select_account = true;

        // "old" was added a month ago, "mid" a week ago.
        store.accounts.insert(
            "old@x.com".into(),
            account_added_at("old@x.com", now - Duration::days(30)),
        );
        store.accounts.insert(
            "mid@x.com".into(),
            account_added_at("mid@x.com", now - Duration::days(7)),
        );
        let none = HashSet::new();

        assert_eq!(
            store
                .auto_account_for_device(Some("d0"), now, &none)
                .map(|a| a.email().as_str()),
            Some("mid@x.com"),
            "before: the more recently added account is picked first"
        );

        // Signing "old" in again from a list: the sign-in stamps it with now.
        store.insert_account(account_added_at("old@x.com", now));

        assert_eq!(store.accounts.len(), 2, "re-adding must not duplicate");
        assert_eq!(
            store.get_account("old@x.com").and_then(GsaAccount::added_at),
            Some(now),
            "the added date is reset, not preserved"
        );
        assert_eq!(
            store
                .auto_account_for_device(Some("d0"), now, &none)
                .map(|a| a.email().as_str()),
            Some("old@x.com"),
            "after: the re-added account is now the newest and picked first"
        );
    }

    #[test]
    fn re_adding_with_different_case_keeps_one_entry_and_its_devices() {
        let now = Utc::now();
        let mut store = store_with(&["User@X.com"]);
        install(&mut store, "d0", "User@X.com", now);

        store.insert_account(account_added_at("user@x.com", now));

        assert_eq!(store.accounts.len(), 1);
        assert!(store.get_account("user@x.com").is_some());
        assert!(store.get_account("User@X.com").is_none());
        // The device it had provisioned follows the account.
        assert_eq!(store.device_account_email("d0"), Some("user@x.com"));
        assert_eq!(store.remaining_device_slots("user@x.com"), 2);
    }

    fn account_in_group(email: &str, group: Option<u32>) -> GsaAccount {
        let mut account = account(email);
        account.set_identity_group(group);
        account
    }

    /// Adds `email` the way a real sign-in does: takes the group the store
    /// proposes at `now` and stores the account stamped with it.
    fn sign_in(store: &mut AccountStore, email: &str, now: DateTime<Utc>) -> Option<u32> {
        let group = store.identity_group_for_sign_in_at(email, now);
        let mut account = account_in_group(email, group);
        account.set_added_at(Some(now));
        store.insert_account(account);
        group
    }

    #[test]
    fn grouping_off_means_the_machine_identity_for_everyone() {
        let mut store = AccountStore::default();
        store
            .accounts
            .insert("grouped@x.com".into(), account_in_group("grouped@x.com", Some(2)));

        assert_eq!(store.identity_group_for_sign_in("new@x.com"), None);
        // Even an account that once had a group: with grouping off it is
        // re-signed under the machine identity.
        assert_eq!(store.identity_group_for_sign_in("grouped@x.com"), None);
    }

    #[test]
    fn groups_fill_in_order_five_at_a_time() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;

        for i in 0..(IDENTITY_GROUP_SIZE * 2 + 1) {
            let group = sign_in(&mut store, &format!("a{i}@x.com"), now);
            assert_eq!(group, Some((i / IDENTITY_GROUP_SIZE) as u32), "account {i}");
        }

        assert_eq!(store.identity_group_recent_sign_ins(0, now), IDENTITY_GROUP_SIZE);
        assert_eq!(store.identity_group_recent_sign_ins(1, now), IDENTITY_GROUP_SIZE);
        assert_eq!(store.identity_group_recent_sign_ins(2, now), 1);
    }

    #[test]
    fn the_rate_per_identity_follows_the_setting() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;

        // Unset is the shipped default, so an old config behaves as before.
        assert_eq!(store.identity_group_size(), IDENTITY_GROUP_SIZE);

        store.set_identity_group_size_sync(2).unwrap();
        assert_eq!(store.identity_group_size(), 2);

        for i in 0..5 {
            let group = sign_in(&mut store, &format!("a{i}@x.com"), now);
            assert_eq!(group, Some((i / 2) as u32), "account {i}");
        }

        // Out-of-range values are pulled back into range rather than refused.
        store
            .set_identity_group_size_sync(MAX_IDENTITY_GROUP_SIZE + 1_000)
            .unwrap();
        assert_eq!(store.identity_group_size(), MAX_IDENTITY_GROUP_SIZE as usize);

        store.set_identity_group_size_sync(0).unwrap();
        assert_eq!(store.identity_group_size(), MIN_IDENTITY_GROUP_SIZE as usize);

        // One per identity: the next new account gets an identity of its own,
        // after the three the accounts above are already sitting on.
        assert_eq!(store.identity_group_for_sign_in_at("fresh@x.com", now), Some(3));
    }

    #[test]
    fn a_group_takes_new_accounts_again_once_its_sign_ins_are_a_day_old() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;

        // Five sign-ins on group 0, 25 hours ago.
        let yesterday = now - Duration::hours(IDENTITY_GROUP_WINDOW_HOURS + 1);
        for i in 0..IDENTITY_GROUP_SIZE {
            sign_in(&mut store, &format!("old{i}@x.com"), yesterday);
        }
        assert_eq!(store.identity_group_members(0).count(), IDENTITY_GROUP_SIZE);

        // Back then it was full: a sixth account would have opened group 1.
        assert_eq!(
            store.identity_group_for_sign_in_at("late@x.com", yesterday),
            Some(1)
        );

        // Today those sign-ins are outside the window, so group 0 is free
        // again -- the limit is a rate, not a lifetime count -- and it holds
        // ten accounts in total without ever exceeding five per day.
        assert_eq!(store.identity_group_recent_sign_ins(0, now), 0);
        for i in 0..IDENTITY_GROUP_SIZE {
            assert_eq!(sign_in(&mut store, &format!("new{i}@x.com"), now), Some(0));
        }
        assert_eq!(store.identity_group_members(0).count(), 2 * IDENTITY_GROUP_SIZE);
        assert_eq!(sign_in(&mut store, "sixth@x.com", now), Some(1));
    }

    #[test]
    fn a_sign_in_just_inside_the_window_still_counts() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;

        let almost_a_day = now - Duration::hours(IDENTITY_GROUP_WINDOW_HOURS) + Duration::minutes(1);
        for i in 0..IDENTITY_GROUP_SIZE {
            sign_in(&mut store, &format!("a{i}@x.com"), almost_a_day);
        }

        assert_eq!(store.identity_group_for_sign_in_at("next@x.com", now), Some(1));
    }

    #[test]
    fn a_re_added_account_keeps_its_group_even_when_the_group_is_at_the_rate() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;
        for i in 0..IDENTITY_GROUP_SIZE {
            sign_in(&mut store, &format!("a{i}@x.com"), now);
        }
        sign_in(&mut store, "b@x.com", now);
        assert_eq!(store.get_account("b@x.com").unwrap().identity_group(), Some(1));

        // Group 0 is at five for today, yet a0 signing in again stays on it:
        // same Apple ID, same "device". Case of the address does not matter.
        assert_eq!(store.identity_group_for_sign_in_at("a0@x.com", now), Some(0));
        assert_eq!(store.identity_group_for_sign_in_at("A0@X.COM", now), Some(0));
        assert_eq!(sign_in(&mut store, "a0@x.com", now), Some(0));
        assert_eq!(store.identity_group_members(0).count(), IDENTITY_GROUP_SIZE);
    }

    #[test]
    fn removing_an_account_and_adding_it_back_places_it_afresh() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;
        for i in 0..IDENTITY_GROUP_SIZE {
            sign_in(&mut store, &format!("a{i}@x.com"), now);
        }
        assert_eq!(store.get_account("a0@x.com").unwrap().identity_group(), Some(0));

        store.forget_account("a0@x.com");

        // Its seat in group 0 is gone with it, but so is its claim on the
        // group: the other four sign-ins today leave one seat, which it takes
        // as a newcomer would. Had group 0 still been at five, it would have
        // gone to group 1 rather than back to its old identity.
        assert_eq!(store.identity_group_recent_sign_ins(0, now), IDENTITY_GROUP_SIZE - 1);
        assert_eq!(sign_in(&mut store, "a0@x.com", now), Some(0));

        store.forget_account("a1@x.com");
        sign_in(&mut store, "filler@x.com", now);
        assert_eq!(store.identity_group_recent_sign_ins(0, now), IDENTITY_GROUP_SIZE);
        assert_eq!(sign_in(&mut store, "a1@x.com", now), Some(1));
    }

    #[test]
    fn accounts_from_before_grouping_are_placed_when_re_added() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;
        store
            .accounts
            .insert("legacy@x.com".into(), account("legacy@x.com"));
        assert_eq!(store.get_account("legacy@x.com").unwrap().identity_group(), None);

        assert_eq!(sign_in(&mut store, "legacy@x.com", now), Some(0));
    }

    #[test]
    fn devices_are_planned_into_one_group_per_account() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        store.selected_account = Some("a@x.com".to_string());
        for (i, email) in ["a@x.com", "b@x.com"].iter().enumerate() {
            store.accounts.insert(
                email.to_string(),
                account_added_at(email, now - Duration::minutes(i as i64)),
            );
        }

        let udids: Vec<String> = (0..7).map(|i| format!("d{i}")).collect();
        let (groups, unassigned) = store.plan_device_accounts_at(&udids, now, &HashSet::new());

        // Three devices per account, then nothing left to offer the seventh.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "a@x.com");
        assert_eq!(groups[0].1, vec!["d0", "d1", "d2"]);
        assert_eq!(groups[1].0, "b@x.com");
        assert_eq!(groups[1].1, vec!["d3", "d4", "d5"]);
        assert_eq!(unassigned, vec!["d6"]);
    }

    #[test]
    fn a_refused_account_is_left_out_of_the_plan() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        store.selected_account = Some("a@x.com".to_string());
        for email in ["a@x.com", "b@x.com"] {
            store
                .accounts
                .insert(email.to_string(), account_added_at(email, now));
        }

        let udids = vec!["d0".to_string(), "d1".to_string()];
        let refused = HashSet::from(["a@x.com".to_string()]);
        let (groups, unassigned) = store.plan_device_accounts_at(&udids, now, &refused);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "b@x.com");
        assert_eq!(groups[0].1, vec!["d0", "d1"]);
        assert!(unassigned.is_empty());
    }

    #[test]
    fn a_device_already_on_an_account_joins_that_account_s_group() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.auto_select_account = true;
        store.selected_account = Some("new@x.com".to_string());
        store
            .accounts
            .insert("old@x.com".into(), account_added_at("old@x.com", now - Duration::days(30)));
        store
            .accounts
            .insert("new@x.com".into(), account_added_at("new@x.com", now));
        // "d9" was provisioned by the older account last week; it stays there.
        install(&mut store, "d9", "old@x.com", now - Duration::days(1));

        let udids = vec!["d0".to_string(), "d9".to_string(), "d1".to_string()];
        let (groups, _) = store.plan_device_accounts_at(&udids, now, &HashSet::new());

        let for_old = groups.iter().find(|(email, _)| email == "old@x.com").unwrap();
        assert_eq!(for_old.1, vec!["d9"]);
        let for_new = groups.iter().find(|(email, _)| email == "new@x.com").unwrap();
        assert_eq!(for_new.1, vec!["d0", "d1"]);
    }

    #[test]
    fn network_settings_default_when_missing_and_round_trip() {
        // A store written before the Connection tab existed has no `network`.
        let old_json = r#"{"selected_account":null,"accounts":{}}"#;
        let store: AccountStore = serde_json::from_str(old_json).unwrap();
        assert_eq!(store.network(), &NetworkSettings::default());
        assert_eq!(store.network().bulk_gap_secs, DEFAULT_BULK_GAP_SECS);
        assert_eq!(store.network().install_parallel, DEFAULT_INSTALL_PARALLEL);
        assert_eq!(store.network().proxy_url, None);

        let mut store = AccountStore::default();
        store.network = NetworkSettings {
            proxy_url: Some("socks5h://u:p@10.0.0.2:1080".into()),
            bulk_gap_secs: 0,
            rate_limit_wait_secs: 5,
            install_parallel: 8,
        };
        let json = serde_json::to_string(&store).unwrap();
        let back: AccountStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.network(), store.network());
    }

    #[test]
    fn last_install_is_stamped_and_outlives_the_device_entries() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);
        assert_eq!(store.last_install_at("a@x.com"), None);

        store
            .record_device_account_at_sync("d0", "a@x.com", now - Duration::days(30))
            .unwrap();
        assert_eq!(
            store.last_install_at("a@x.com"),
            Some(now - Duration::days(30))
        );

        // A newer device moves the date forward; an older one cannot pull it
        // back, because a batch records devices in whatever order they finish.
        store
            .record_device_account_at_sync("d1", "a@x.com", now)
            .unwrap();
        store
            .record_device_account_at_sync("d2", "a@x.com", now - Duration::days(1))
            .unwrap();
        assert_eq!(store.last_install_at("a@x.com"), Some(now));

        // The 30-day-old device entry ages out; the date does not.
        store.tidy_device_entries(now);
        assert_eq!(store.remaining_device_slots("a@x.com"), 1);
        assert_eq!(store.last_install_at("a@x.com"), Some(now));
    }

    #[test]
    fn re_adding_an_account_keeps_its_install_history() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);
        store
            .record_device_account_at_sync("d0", "a@x.com", now)
            .unwrap();

        // What a fresh sign-in hands over: a new record with no history.
        store.insert_account(account("a@x.com"));
        assert_eq!(store.last_install_at("a@x.com"), Some(now));

        // Same account in another case is the same account here too.
        store.insert_account(account("A@X.com"));
        assert_eq!(store.last_install_at("A@X.com"), Some(now));
    }

    #[test]
    fn an_account_from_before_the_date_existed_takes_it_from_its_devices() {
        let now = Utc::now();
        let mut store = store_with(&["a@x.com"]);
        // Written the way an older build did: device entry, no stamp.
        install(&mut store, "d0", "a@x.com", now - Duration::days(4));
        install(&mut store, "d1", "a@x.com", now - Duration::days(2));
        assert_eq!(store.last_install_at("a@x.com"), None);

        store.tidy_device_entries(now);
        assert_eq!(
            store.last_install_at("a@x.com"),
            Some(now - Duration::days(2)),
            "the newest of the account's devices"
        );
    }

    #[test]
    fn undated_members_do_not_count_against_the_rate() {
        let now = Utc::now();
        let mut store = AccountStore::default();
        store.group_identities = true;
        for i in 0..IDENTITY_GROUP_SIZE {
            let email = format!("undated{i}@x.com");
            let mut account = account_in_group(&email, Some(0));
            account.set_added_at(None);
            store.accounts.insert(email, account);
        }

        assert_eq!(store.identity_group_recent_sign_ins(0, now), 0);
        assert_eq!(sign_in(&mut store, "new@x.com", now), Some(0));
    }
}
