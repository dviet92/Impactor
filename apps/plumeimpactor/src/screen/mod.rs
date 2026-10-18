pub(crate) mod general;
mod package;
mod progress;
pub(crate) mod settings;
mod utilties;
mod windows;

use std::collections::VecDeque;

use iced::Length::Fill;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, stack, text, text_editor,
};
use iced::window;
use iced::{Element, Subscription, Task};

use plume_store::AccountStore;
use plume_utils::{Device, SignerOptions};

use crate::subscriptions;
use crate::tray::ImpactorTray;
use crate::{appearance, defaults};
use windows::login_window;

static REFRESH_DAEMON_DEVICES: std::sync::OnceLock<crate::refresh::ConnectedDevices> =
    std::sync::OnceLock::new();

pub fn set_refresh_daemon_devices(devices: crate::refresh::ConnectedDevices) {
    let _ = REFRESH_DAEMON_DEVICES.set(devices);
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    // Navigation
    NavigateToScreen(ImpactorScreenType),
    NextScreen,
    PreviousScreen,

    // Device management
    ComboBoxSelected(String),
    DeviceConnected(Device),
    DeviceDisconnected(u32),

    // Tray
    TrayMenuClicked(tray_icon::menu::MenuId),
    TrayIconClicked,
    #[cfg(target_os = "linux")]
    GtkTick,
    #[cfg(target_os = "macos")]
    MacOsActivationTick,

    // Refresh operations
    RefreshAppNow {
        udid: String,
        app_path: String,
    },
    ForgetApp {
        udid: String,
        app_path: String,
    },
    UpdateTrayMenu,

    // Keyboard shortcuts that no widget consumed, with the window they came from
    HotKey(HotKey, window::Id),

    // Window management
    RelaunchRequested,
    ShowWindow,
    HideWindow,
    Quit,

    // Login window
    LoginWindowMessage(window::Id, login_window::Message),

    // Screen-specific messages
    MainScreen(general::Message),
    UtilitiesScreen(utilties::Message),
    SettingsScreen(settings::Message),
    InstallerScreen(package::Message),
    ProgressScreen(progress::Message),
    CertificateResetRequested(crate::certificate_reset::ConfirmationRequest),
    ConfirmCertificateReset,
    CancelCertificateReset,

    // Installation
    StartInstallation,

    // Multi install
    MultiInstallToggleDevice(String),
    MultiInstallSetAll(bool),
    /// Tick exactly the devices the last run did not finish: the ones that
    /// failed, and the ones never installed at all.
    MultiInstallSelectUnfinished,
    MultiInstallConfirm,
    MultiInstallCancel,

    // Bulk account add
    BulkAddShow,
    BulkAddEdit(text_editor::Action),
    BulkAddConfirm,
    BulkAddCancel,
    BulkAddNext,
    BulkAddOpen(BulkCredential, usize, usize),
}

/// Fallback pause between sign-ins during a bulk run, when the store has none.
///
/// Signing several Apple IDs in from one machine back to back looks like abuse
/// to Apple: the sessions get invalidated and the accounts can be locked, which
/// then needs a manual unlock on appleid.apple.com. Spacing the attempts out
/// costs a couple of minutes and avoids that. The value actually used is the
/// user's, from the Connection tab: with a proxy pool in front there is no
/// shared counter to respect and it can be zero.
const BULK_LOGIN_GAP: std::time::Duration =
    std::time::Duration::from_secs(plume_store::DEFAULT_BULK_GAP_SECS);

/// The gap stops growing here after repeated rate limits.
const BULK_LOGIN_GAP_MAX: std::time::Duration = std::time::Duration::from_secs(240);

/// After a 429 the gap at least becomes this, so a gap of zero (proxy users)
/// still backs off instead of doubling zero forever.
const BULK_LOGIN_GAP_AFTER_429_MIN: std::time::Duration = std::time::Duration::from_secs(15);

/// Keys handled at the application level when no widget took them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotKey {
    /// Remove the ticked account, on the Settings > Accounts page.
    Delete,
    /// Move focus to the next text field -- the login window's email box to
    /// its password box.
    Tab,
    ShiftTab,
}

/// Orders devices by OS version, then UDID.
///
/// The version is compared as numbers, not text: as text "9.3" sorts after
/// "18.5", and the same breaks for any component that reaches double digits.
/// Devices whose version could not be read sort last rather than first, so a
/// failed lockdown query does not push one to the top of the list. UDID breaks
/// ties and is what makes the order survive a replug.
fn device_sort_key(device: &Device) -> (bool, Vec<u32>, &str) {
    let version = device.os_version.as_deref();

    let numbers = version
        .map(|version| {
            version
                .split('.')
                .map(|part| part.trim().parse::<u32>().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default();

    (version.is_none(), numbers, device.udid.as_str())
}

/// One line of the pasted account list.
#[derive(Clone)]
pub struct BulkCredential {
    email: String,
    password: String,
}

impl std::fmt::Debug for BulkCredential {
    /// Hand written so a derived `Debug` can never put a password in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BulkCredential")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Splits a pasted list into credentials, one per line.
///
/// A line is `email|password` or `email<TAB>password`, split at the first
/// separator so a password containing `|` survives (an email cannot contain
/// one). Blank lines and `#` comments are ignored. Whitespace is trimmed off
/// both halves, which matters because lists pasted on Windows carry a trailing
/// `\r` on every line.
///
/// Returns the credentials in file order plus the lines that could not be read,
/// so the user is told which ones were dropped instead of silently losing them.
fn parse_bulk_credentials(input: &str) -> (Vec<BulkCredential>, Vec<String>) {
    let mut credentials = Vec::new();
    let mut malformed = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in input.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(split_at) = line.find(['|', '\t']) else {
            malformed.push(line.to_string());
            continue;
        };

        let (email, password) = line.split_at(split_at);
        let email = email.trim();
        let password = password[1..].trim();

        if email.is_empty() || password.is_empty() {
            malformed.push(line.to_string());
            continue;
        }

        // Apple treats the address case-insensitively, so a list holding the
        // same account twice in different cases is still a duplicate.
        if !seen.insert(email.to_ascii_lowercase()) {
            continue;
        }

        credentials.push(BulkCredential {
            email: email.to_string(),
            password: password.to_string(),
        });
    }

    (credentials, malformed)
}

/// Open state of the paste box.
#[derive(Default)]
struct BulkAddPrompt {
    content: text_editor::Content,
    error: Option<String>,
}

/// A bulk add in progress: what is left, and what happened so far.
#[derive(Debug, Default)]
struct BulkAddQueue {
    pending: VecDeque<BulkCredential>,
    /// How many accounts were queued, for the "3 of 7" header.
    total: usize,
    current: Option<String>,
    /// The credential being signed in, kept so a rate-limited attempt can be
    /// put back on the queue instead of being written off.
    current_credential: Option<BulkCredential>,
    /// Accounts already given a second go after a rate limit. One retry each:
    /// a second 429 in a row means the wait is still too short, and the
    /// summary should say so rather than loop.
    requeued: std::collections::HashSet<String>,
    /// Current pause between sign-ins. Starts at the user's setting and
    /// doubles each time Apple answers 429, since that is Apple saying the
    /// gap was not long enough.
    gap: std::time::Duration,
    added: Vec<String>,
    failed: Vec<String>,
    cancelled: bool,
}

impl BulkAddQueue {
    /// Whether a sign-in failure was Apple's sign-in rate limit rather than
    /// anything about the account. Matched on the message plume_core builds
    /// for [`plume_core::Error::RateLimited`].
    fn is_rate_limit(error: &str) -> bool {
        error.contains("HTTP 429") || error.contains("rate limit")
    }
}

impl BulkAddQueue {
    /// What to show once the run stops, either finished or cancelled.
    fn summary(&self) -> String {
        let mut lines = vec![rust_i18n::t!(
            "bulk_add_summary_counts",
            added = self.added.len().to_string(),
            failed = self.failed.len().to_string()
        )
        .to_string()];

        if !self.failed.is_empty() {
            lines.push(String::new());
            lines.push(rust_i18n::t!("bulk_add_summary_failed").to_string());
            lines.extend(self.failed.iter().map(|entry| format!("  {entry}")));
        }

        if !self.pending.is_empty() {
            lines.push(String::new());
            lines.push(
                rust_i18n::t!(
                    "bulk_add_summary_not_attempted",
                    count = self.pending.len().to_string()
                )
                .to_string(),
            );
            lines.extend(
                self.pending
                    .iter()
                    .map(|credential| format!("  {}", credential.email)),
            );
        }

        lines.join("\n")
    }
}

/// Open state of the multi-install picker: which devices are ticked.
#[derive(Debug, Default)]
struct MultiInstallPrompt {
    selected: std::collections::HashSet<String>,
}

pub struct Impactor {
    current_screen: ImpactorScreen,
    previous_screen: Option<Box<ImpactorScreen>>,
    devices: Vec<Device>,
    selected_device: Option<Device>,
    tray: Option<ImpactorTray>,
    main_window: Option<window::Id>,
    account_store: Option<AccountStore>,
    login_windows: std::collections::HashMap<window::Id, login_window::LoginWindow>,
    pending_installation: bool,
    certificate_reset_queue: VecDeque<crate::certificate_reset::ConfirmationRequest>,
    /// Installer screen kept alive while an install runs, so leaving the progress
    /// screen returns to the loaded package instead of the empty drop zone.
    parked_installer: Option<package::PackageScreen>,
    install_in_flight: bool,
    multi_install: Option<MultiInstallPrompt>,
    bulk_add: Option<BulkAddPrompt>,
    bulk_queue: Option<BulkAddQueue>,
    bulk_waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImpactorScreenType {
    Main,
    Utilities,
    Settings,
    Installer,
    Progress,
}

enum ImpactorScreen {
    Main(general::GeneralScreen),
    Utilities(utilties::UtilitiesScreen),
    Settings(settings::SettingsScreen),
    Installer(package::PackageScreen),
    Progress(progress::ProgressScreen),
}

impl Impactor {
    pub fn new() -> (Self, Task<Message>) {
        let mut tray = ImpactorTray::new();
        let store = Self::init_account_store_sync();
        tray.update_refresh_apps(&store);
        // Everything Apple-facing reads these globals; install the saved
        // choice before any window can start a sign-in.
        Self::apply_network(store.network());
        let start_in_tray = crate::startup::start_in_tray_from_args();
        let (main_window, open_task) = if start_in_tray {
            (None, Task::none())
        } else {
            let (id, open_task) = window::open(defaults::default_window_settings());
            (Some(id), open_task.discard())
        };
        crate::macos_app::set_main_window_visible(true);
        crate::macos_app::reset_activation_state();

        (
            Self {
                current_screen: ImpactorScreen::Main(general::GeneralScreen::new()),
                previous_screen: None,
                devices: Vec::new(),
                selected_device: None,
                tray: Some(tray),
                main_window,
                account_store: Some(store),
                login_windows: std::collections::HashMap::new(),
                pending_installation: false,
                certificate_reset_queue: VecDeque::new(),
                parked_installer: None,
                install_in_flight: false,
                multi_install: None,
                bulk_add: None,
                bulk_queue: None,
                bulk_waiting: false,
            },
            open_task,
        )
    }

    fn init_account_store_sync() -> AccountStore {
        let path = defaults::get_data_path().join("accounts.json");
        AccountStore::load_sync(&Some(path)).unwrap_or_default()
    }

    fn respond_to_next_certificate_reset(&mut self, accepted: bool) {
        if let Some(request) = self.certificate_reset_queue.pop_front() {
            request.respond(accepted);
        }
    }

    fn cancel_pending_certificate_resets(&mut self) {
        while let Some(request) = self.certificate_reset_queue.pop_front() {
            request.respond(false);
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ComboBoxSelected(value) => {
                self.selected_device = self
                    .devices
                    .iter()
                    .find(|d| d.to_string() == value)
                    .cloned();

                if let ImpactorScreen::Utilities(_) = self.current_screen {
                    let rppairing_enabled = match &self.current_screen {
                        ImpactorScreen::Utilities(screen) => screen.rppairing_enabled,
                        _ => false,
                    };
                    self.current_screen = ImpactorScreen::Utilities(
                        utilties::UtilitiesScreen::new(self.selected_device.clone()),
                    );
                    return Task::done(Message::UtilitiesScreen(utilties::Message::RefreshApps(
                        rppairing_enabled,
                    )));
                }

                Task::none()
            }
            Message::DeviceConnected(device) => {
                if !self.devices.iter().any(|d| d.device_id == device.device_id) {
                    self.devices.push(device.clone());

                    // Keep one stable order everywhere devices are listed. They
                    // arrive in whatever order they are detected, so unplugging
                    // and replugging one otherwise shuffles the multi-install
                    // list and the picker under the user.
                    self.devices
                        .sort_by(|a, b| device_sort_key(a).cmp(&device_sort_key(b)));

                    if self.selected_device.is_none() && device.device_id != u32::MAX {
                        self.selected_device = Some(device.clone());
                    }
                }

                if let Some(daemon_devices) = REFRESH_DAEMON_DEVICES.get() {
                    if let Ok(mut devices) = daemon_devices.lock() {
                        devices.insert(device.udid.clone(), device.clone());
                    }
                }

                if let ImpactorScreen::Utilities(_) = self.current_screen {
                    let rppairing_enabled = match &self.current_screen {
                        ImpactorScreen::Utilities(screen) => screen.rppairing_enabled,
                        _ => false,
                    };
                    self.current_screen = ImpactorScreen::Utilities(
                        utilties::UtilitiesScreen::new(self.selected_device.clone()),
                    );
                    return Task::done(Message::UtilitiesScreen(utilties::Message::RefreshApps(
                        rppairing_enabled,
                    )));
                }

                Task::none()
            }
            Message::DeviceDisconnected(id) => {
                let udid = self
                    .devices
                    .iter()
                    .find(|d| d.device_id == id)
                    .map(|d| d.udid.clone());

                self.devices.retain(|d| d.device_id != id);

                if self.selected_device.as_ref().map(|d| d.device_id) == Some(id) {
                    self.selected_device = self.devices.first().cloned();
                }

                if let (Some(udid), Some(daemon_devices)) = (udid, REFRESH_DAEMON_DEVICES.get()) {
                    if let Ok(mut devices) = daemon_devices.lock() {
                        devices.remove(&udid);
                    }
                }

                if let ImpactorScreen::Utilities(_) = self.current_screen {
                    let rppairing_enabled = match &self.current_screen {
                        ImpactorScreen::Utilities(screen) => screen.rppairing_enabled,
                        _ => false,
                    };
                    self.current_screen = ImpactorScreen::Utilities(
                        utilties::UtilitiesScreen::new(self.selected_device.clone()),
                    );
                    return Task::done(Message::UtilitiesScreen(utilties::Message::RefreshApps(
                        rppairing_enabled,
                    )));
                }

                Task::none()
            }
            Message::NavigateToScreen(screen_type) => {
                if screen_type == ImpactorScreenType::Settings {
                    if !matches!(self.current_screen, ImpactorScreen::Progress(_)) {
                        self.previous_screen = Some(Box::new(std::mem::replace(
                            &mut self.current_screen,
                            ImpactorScreen::Main(general::GeneralScreen::new()),
                        )));
                    }
                }

                self.navigate_to_screen(screen_type.clone());

                if screen_type == ImpactorScreenType::Utilities {
                    let rppairing_enabled = match &self.current_screen {
                        ImpactorScreen::Utilities(screen) => screen.rppairing_enabled,
                        _ => false,
                    };
                    return Task::done(Message::UtilitiesScreen(utilties::Message::RefreshApps(
                        rppairing_enabled,
                    )));
                }

                Task::none()
            }
            Message::NextScreen => {
                let next_screen = match self.current_screen {
                    ImpactorScreen::Main(_) => ImpactorScreenType::Installer,
                    ImpactorScreen::Utilities(_) => return Task::none(),
                    ImpactorScreen::Installer(_) => ImpactorScreenType::Progress,
                    ImpactorScreen::Settings(_) => return Task::none(),
                    ImpactorScreen::Progress(_) => return Task::none(),
                };

                self.navigate_to_screen(next_screen);
                Task::none()
            }
            Message::PreviousScreen => match &self.current_screen {
                ImpactorScreen::Main(_) => Task::none(),
                ImpactorScreen::Utilities(_) => {
                    self.navigate_to_screen(ImpactorScreenType::Main);
                    Task::none()
                }
                ImpactorScreen::Installer(_) => {
                    self.navigate_to_screen(ImpactorScreenType::Main);
                    Task::none()
                }
                ImpactorScreen::Progress(_) => {
                    // The package is still staged, so the user can pick another
                    // device and install the same app again without reloading it.
                    match self.parked_installer.take() {
                        Some(installer) => {
                            self.current_screen = ImpactorScreen::Installer(installer)
                        }
                        None => self.navigate_to_screen(ImpactorScreenType::Main),
                    }
                    Task::none()
                }
                ImpactorScreen::Settings(_) => {
                    if let Some(prev_screen) = self.previous_screen.take() {
                        self.current_screen = *prev_screen;
                    } else {
                        self.navigate_to_screen(ImpactorScreenType::Main);
                    }
                    Task::none()
                }
            },
            Message::TrayIconClicked => Task::done(Message::ShowWindow),
            Message::TrayMenuClicked(id) => {
                if let Some(tray) = &self.tray {
                    if let Some(action) = tray.get_action(&id) {
                        match action {
                            crate::tray::TrayAction::Show => Task::done(Message::ShowWindow),
                            crate::tray::TrayAction::Quit => Task::done(Message::Quit),
                            crate::tray::TrayAction::RefreshApp { udid, app_path } => {
                                Task::done(Message::RefreshAppNow {
                                    udid: udid.clone(),
                                    app_path: app_path.clone(),
                                })
                            }
                            crate::tray::TrayAction::ForgetApp { udid, app_path } => {
                                Task::done(Message::ForgetApp {
                                    udid: udid.clone(),
                                    app_path: app_path.clone(),
                                })
                            }
                        }
                    } else {
                        Task::none()
                    }
                } else {
                    Task::none()
                }
            }
            #[cfg(target_os = "linux")]
            Message::GtkTick => {
                while gtk::glib::MainContext::default().iteration(false) {}
                Task::none()
            }
            #[cfg(target_os = "macos")]
            Message::MacOsActivationTick => {
                if self.main_window.is_none() && crate::macos_app::activation_reopen_requested() {
                    Task::done(Message::RelaunchRequested)
                } else {
                    Task::none()
                }
            }
            Message::RelaunchRequested => {
                if self.main_window.is_none() {
                    Task::done(Message::ShowWindow)
                } else {
                    Task::none()
                }
            }
            Message::CertificateResetRequested(request) => {
                self.certificate_reset_queue.push_back(request);
                if self.main_window.is_none() {
                    Task::done(Message::ShowWindow)
                } else {
                    Task::none()
                }
            }
            Message::ConfirmCertificateReset => {
                self.respond_to_next_certificate_reset(true);
                Task::none()
            }
            Message::CancelCertificateReset => {
                self.respond_to_next_certificate_reset(false);
                Task::none()
            }
            Message::ShowWindow => {
                crate::macos_app::set_main_window_visible(true);
                if let Some(id) = self.main_window {
                    window::gain_focus(id)
                } else {
                    let (id, open_task) = window::open(defaults::default_window_settings());
                    self.main_window = Some(id);
                    open_task.discard()
                }
            }
            Message::HideWindow => {
                if let Some(id) = self.main_window {
                    self.cancel_pending_certificate_resets();
                    self.main_window = None;
                    crate::macos_app::set_main_window_visible(false);
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::Quit => {
                self.tray.take();
                std::process::exit(0);
            }
            Message::HotKey(hotkey, id) => match hotkey {
                HotKey::Delete => {
                    // Main window, Settings, Accounts page -- anywhere else
                    // Delete means nothing.
                    let on_accounts_page = matches!(
                        &self.current_screen,
                        ImpactorScreen::Settings(screen) if screen.tab() == settings::Tab::Accounts
                    );
                    if Some(id) == self.main_window && on_accounts_page {
                        Task::done(Message::SettingsScreen(settings::Message::RemoveSelected))
                    } else {
                        Task::none()
                    }
                }
                // Focus operations run across every open window; the main
                // window has nothing focusable on most screens, so in practice
                // this walks the login window's email -> password fields.
                HotKey::Tab => iced::widget::operation::focus_next(),
                HotKey::ShiftTab => iced::widget::operation::focus_previous(),
            },
            Message::LoginWindowMessage(id, msg) => {
                if let Some(login_window) = self.login_windows.get_mut(&id) {
                    let task = login_window.update(msg.clone());

                    // During a bulk run every outcome is terminal for that
                    // account: the window drops its two-factor channel on
                    // failure, so there is nothing left to retry in place.
                    if self.bulk_queue.is_some() {
                        let advance = match &msg {
                            login_window::Message::LoginSuccess(account) => {
                                let email = account.email().clone();
                                if let Some(queue) = &mut self.bulk_queue {
                                    queue.current = None;
                                    queue.current_credential = None;
                                    queue.added.push(email);
                                }
                                true
                            }
                            login_window::Message::LoginFailed(error) => {
                                let error = error.clone();
                                if let Some(queue) = &mut self.bulk_queue {
                                    let credential = queue.current_credential.take();
                                    if let Some(email) = queue.current.take() {
                                        // A 429 is not this account failing, it
                                        // is the list going too fast. Slow down
                                        // and give the account one more turn at
                                        // the end, once the others have gone.
                                        let retry = BulkAddQueue::is_rate_limit(&error)
                                            && !queue.requeued.contains(&email);
                                        match (retry, credential) {
                                            (true, Some(credential)) => {
                                                queue.gap = (queue.gap * 2)
                                                    .max(BULK_LOGIN_GAP_AFTER_429_MIN)
                                                    .min(BULK_LOGIN_GAP_MAX);
                                                log::warn!(
                                                    "bulk add: rate limited on {}; gap now {}s, account re-queued",
                                                    login_window::redact_email(&email),
                                                    queue.gap.as_secs()
                                                );
                                                queue.requeued.insert(email);
                                                queue.pending.push_back(credential);
                                            }
                                            _ => queue.failed.push(format!("{email}: {error}")),
                                        }
                                    }
                                }
                                true
                            }
                            login_window::Message::LoginCancel
                            | login_window::Message::TwoFactorCancel => {
                                // Cancel stops the whole list, not just this
                                // account; the rest are reported as untouched.
                                if let Some(queue) = &mut self.bulk_queue {
                                    queue.current = None;
                                    queue.cancelled = true;
                                }
                                true
                            }
                            _ => false,
                        };

                        if advance {
                            self.login_windows.remove(&id);
                            self.account_store = Some(Self::init_account_store_sync());

                            return Task::batch(vec![
                                window::close(id),
                                Task::done(Message::BulkAddNext),
                            ]);
                        }
                    }

                    if matches!(
                        msg,
                        login_window::Message::LoginSuccess(_)
                            | login_window::Message::LoginCancel
                            | login_window::Message::TwoFactorCancel
                    ) {
                        self.login_windows.remove(&id);
                        self.account_store = Some(Self::init_account_store_sync());

                        if let ImpactorScreen::Settings(_) = self.current_screen {
                            self.current_screen =
                                ImpactorScreen::Settings(settings::SettingsScreen::new());
                        }

                        if self.pending_installation {
                            if matches!(msg, login_window::Message::LoginSuccess(_)) {
                                self.pending_installation = false;
                                let resume = if self.multi_install.is_some() {
                                    Message::MultiInstallConfirm
                                } else {
                                    Message::InstallerScreen(
                                        package::Message::RequestInstallation,
                                    )
                                };
                                return Task::batch(vec![
                                    window::close(id),
                                    Task::done(resume),
                                ]);
                            }
                        }

                        return window::close(id);
                    }

                    task.map(move |msg| Message::LoginWindowMessage(id, msg))
                } else {
                    Task::none()
                }
            }
            Message::MainScreen(msg) => {
                if let ImpactorScreen::Main(ref mut screen) = self.current_screen {
                    let task = screen.update(msg.clone()).map(Message::MainScreen);

                    if let general::Message::NavigateToInstaller(package) = msg {
                        let mut options = SignerOptions::default();
                        package.load_into_signer_options(&mut options);
                        self.current_screen = ImpactorScreen::Installer(
                            package::PackageScreen::new(Some(package), options),
                        );
                    } else if let general::Message::NavigateToUtilities = msg {
                        let rppairing_enabled = match &self.current_screen {
                            ImpactorScreen::Utilities(screen) => screen.rppairing_enabled,
                            _ => false,
                        };
                        self.current_screen = ImpactorScreen::Utilities(
                            utilties::UtilitiesScreen::new(self.selected_device.clone()),
                        );
                        return Task::done(Message::UtilitiesScreen(
                            utilties::Message::RefreshApps(rppairing_enabled),
                        ));
                    }

                    task
                } else {
                    Task::none()
                }
            }
            Message::UtilitiesScreen(msg) => {
                if let ImpactorScreen::Utilities(ref mut screen) = self.current_screen {
                    screen.update(msg).map(Message::UtilitiesScreen)
                } else {
                    Task::none()
                }
            }
            Message::SettingsScreen(msg) => {
                if let ImpactorScreen::Settings(ref mut screen) = self.current_screen {
                    match msg {
                        settings::Message::ShowLogin => {
                            let (login_window, task) = login_window::LoginWindow::new();
                            let id = login_window.window_id().unwrap();
                            self.login_windows.insert(id, login_window);
                            task.map(move |msg| Message::LoginWindowMessage(id, msg))
                        }
                        settings::Message::SelectAccount(index) => {
                            if let Some(store) = &mut self.account_store {
                                let mut emails: Vec<_> = store.accounts().keys().cloned().collect();
                                emails.sort();
                                if let Some(email) = emails.get(index) {
                                    let _ = store.account_select_sync(email);
                                }
                            }
                            Task::none()
                        }
                        settings::Message::RemoveAccount(index) => {
                            if let Some(store) = &mut self.account_store {
                                let mut emails: Vec<_> = store.accounts().keys().cloned().collect();
                                emails.sort();
                                if let Some(email) = emails.get(index) {
                                    let _ = store.accounts_remove_sync(email);
                                }
                            }
                            Task::none()
                        }
                        settings::Message::ToggleAccountFullMark(index) => {
                            // Marked accounts read as full to auto-pick for a
                            // week, which is what makes it reach past an Apple
                            // ID that signs but whose apps will not open.
                            if let Some(store) = &mut self.account_store {
                                let mut emails: Vec<_> = store.accounts().keys().cloned().collect();
                                emails.sort();
                                if let Some(email) = emails.get(index) {
                                    let result = if store.account_is_marked_full(email) {
                                        store.clear_account_full_mark_sync(email)
                                    } else {
                                        store.mark_account_full_sync(email)
                                    };
                                    if let Err(err) = result {
                                        log::error!("Failed to save the full mark: {err}");
                                    }
                                }
                            }
                            Task::none()
                        }
                        settings::Message::ExportP12 => {
                            if let Some(account) = self
                                .account_store
                                .as_ref()
                                .and_then(|s| s.selected_account().cloned())
                            {
                                std::thread::spawn(move || {
                                    let rt = tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                        .unwrap();

                                    let _ = rt.block_on(async move {
                                        crate::subscriptions::export_certificate(account).await
                                    });
                                });
                            }
                            Task::none()
                        }
                        settings::Message::ToggleAutoStart(enabled) => {
                            if let Err(err) = crate::startup::set_auto_start_enabled(enabled) {
                                log::error!("Failed to update auto-start: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::FetchTeams(ref email) => {
                            if let Some(account_store) = &self.account_store {
                                if let Some(account) = account_store.accounts().get(email) {
                                    let account_clone = account.clone();
                                    let email_clone = email.clone();

                                    return Task::perform(
                                        async move {
                                            let (tx, rx) = std::sync::mpsc::channel();

                                            std::thread::spawn(move || {
                                                let rt = tokio::runtime::Runtime::new().unwrap();
                                                let result = rt.block_on(async move {
                                                    crate::subscriptions::fetch_teams(
                                                        &account_clone,
                                                    )
                                                    .await
                                                    .unwrap_or_else(|e| {
                                                        log::error!("Failed to fetch teams: {}", e);
                                                        Vec::new()
                                                    })
                                                });
                                                let _ = tx.send(result);
                                            });

                                            rx.recv().unwrap_or_default()
                                        },
                                        move |teams| {
                                            Message::SettingsScreen(settings::Message::TeamsLoaded(
                                                email_clone,
                                                teams,
                                            ))
                                        },
                                    );
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::SelectTeam(ref email, ref team_id) => {
                            if let Some(store) = &mut self.account_store {
                                if let Err(e) =
                                    store.update_account_team_sync(email, team_id.clone())
                                {
                                    log::error!("Failed to update team: {:?}", e);
                                } else {
                                    self.account_store = Some(Self::init_account_store_sync());
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::ShowBulkAdd => Task::done(Message::BulkAddShow),
                        settings::Message::ToggleAutoAccount(enabled) => {
                            if let Some(store) = &mut self.account_store {
                                if let Err(err) = store.set_auto_select_account_sync(enabled) {
                                    log::error!("Failed to persist auto account setting: {err}");
                                }
                            }
                            Task::none()
                        }
                        settings::Message::ToggleGroupIdentities(enabled) => {
                            if let Some(store) = &mut self.account_store {
                                if let Err(err) = store.set_group_identities_sync(enabled) {
                                    log::error!(
                                        "Failed to persist identity grouping setting: {err}"
                                    );
                                }
                            }
                            Task::none()
                        }
                        settings::Message::RemoveSelected => {
                            if let Some(store) = &mut self.account_store {
                                if let Some(email) =
                                    store.selected_account().map(|account| account.email().clone())
                                {
                                    if let Err(err) = store.accounts_remove_sync(&email) {
                                        log::error!("Failed to remove account: {err}");
                                    }
                                }
                            }
                            Task::none()
                        }
                        settings::Message::ProxyApply => {
                            // Validate again here rather than trusting the
                            // screen's live check: this is the point where the
                            // value is written to disk and handed to every
                            // client.
                            let draft = screen.proxy_draft().map(str::to_string);
                            let valid = match &draft {
                                Some(url) => plume_core::net::validate_proxy(url).is_ok(),
                                None => true,
                            };
                            if valid {
                                if let Some(store) = &mut self.account_store {
                                    let mut network = store.network().clone();
                                    network.proxy_url = draft;
                                    if let Err(err) = store.set_network_sync(network) {
                                        log::error!("Failed to persist proxy: {err}");
                                    }
                                    Self::apply_network(store.network());
                                }
                                screen.update(msg).map(Message::SettingsScreen)
                            } else {
                                Task::none()
                            }
                        }
                        settings::Message::ProxyClear => {
                            if let Some(store) = &mut self.account_store {
                                let mut network = store.network().clone();
                                network.proxy_url = None;
                                if let Err(err) = store.set_network_sync(network) {
                                    log::error!("Failed to persist proxy: {err}");
                                }
                                Self::apply_network(store.network());
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::ProxyTest => {
                            // reqwest needs a tokio reactor, which iced's
                            // executor is not, so the probe runs on its own
                            // thread and reports back through a oneshot.
                            let (tx, rx) =
                                iced::futures::channel::oneshot::channel::<Result<String, String>>();
                            std::thread::spawn(move || {
                                let result = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt.block_on(plume_core::net::test_connection()),
                                    Err(e) => Err(format!("could not start runtime: {e}")),
                                };
                                let _ = tx.send(result);
                            });
                            let started = screen.update(msg).map(Message::SettingsScreen);
                            Task::batch(vec![
                                started,
                                Task::perform(
                                    async move {
                                        rx.await.unwrap_or_else(|_| {
                                            Err("the test stopped without a result".to_string())
                                        })
                                    },
                                    |result| {
                                        Message::SettingsScreen(settings::Message::ProxyTested(
                                            result,
                                        ))
                                    },
                                ),
                            ])
                        }
                        settings::Message::BulkGapEdited(ref text) => {
                            // Saved on every valid keystroke; an unparsable
                            // draft (an emptied box) just stays on screen.
                            if let Ok(seconds) = text.trim().parse::<u64>() {
                                if let Some(store) = &mut self.account_store {
                                    let mut network = store.network().clone();
                                    network.bulk_gap_secs = seconds;
                                    if let Err(err) = store.set_network_sync(network) {
                                        log::error!("Failed to persist bulk gap: {err}");
                                    }
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::IdentityGroupSizeEdited(ref text) => {
                            // Saved on every valid keystroke, like the other
                            // number boxes; an emptied box just stays empty
                            // until something parseable is typed.
                            if let Ok(size) = text.trim().parse::<u64>() {
                                if let Some(store) = &mut self.account_store {
                                    if let Err(err) = store.set_identity_group_size_sync(size) {
                                        log::error!("Failed to persist identity group size: {err}");
                                    }
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::InstallParallelEdited(ref text) => {
                            if let Ok(devices) = text.trim().parse::<u64>() {
                                if let Some(store) = &mut self.account_store {
                                    let mut network = store.network().clone();
                                    network.install_parallel = devices;
                                    if let Err(err) = store.set_network_sync(network) {
                                        log::error!("Failed to persist install parallelism: {err}");
                                    }
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        settings::Message::RateLimitWaitEdited(ref text) => {
                            if let Ok(seconds) = text.trim().parse::<u64>() {
                                if let Some(store) = &mut self.account_store {
                                    let mut network = store.network().clone();
                                    network.rate_limit_wait_secs = seconds;
                                    if let Err(err) = store.set_network_sync(network) {
                                        log::error!("Failed to persist rate-limit wait: {err}");
                                    }
                                    Self::apply_network(store.network());
                                }
                            }
                            screen.update(msg).map(Message::SettingsScreen)
                        }
                        _ => screen.update(msg).map(Message::SettingsScreen),
                    }
                } else {
                    Task::none()
                }
            }
            Message::InstallerScreen(msg) => {
                if let ImpactorScreen::Installer(ref mut screen) = self.current_screen {
                    match msg {
                        package::Message::Back => Task::done(Message::PreviousScreen),
                        package::Message::RequestInstallation => {
                            if screen.selected_package.is_none() {
                                return Task::none();
                            }

                            use plume_utils::SignerMode;
                            if matches!(screen.options.mode, SignerMode::Pem) {
                                if self.account_for_selected_device().is_none() {
                                    self.pending_installation = true;

                                    let (login_window, task) = login_window::LoginWindow::new();
                                    let id = login_window.window_id().unwrap();
                                    self.login_windows.insert(id, login_window);
                                    return task
                                        .map(move |msg| Message::LoginWindowMessage(id, msg));
                                }
                            }

                            self.start_installation_task()
                        }
                        package::Message::RequestMultiInstallation => {
                            if screen.selected_package.is_none() {
                                return Task::none();
                            }

                            let mut selected = std::collections::HashSet::new();
                            if let Some(device) = &self.selected_device {
                                selected.insert(device.udid.clone());
                            }

                            self.multi_install = Some(MultiInstallPrompt { selected });
                            Task::none()
                        }
                        _ => screen.update(msg).map(Message::InstallerScreen),
                    }
                } else {
                    Task::none()
                }
            }
            Message::ProgressScreen(msg) => {
                if let ImpactorScreen::Progress(ref mut screen) = self.current_screen {
                    match msg {
                        progress::Message::Back => Task::done(Message::PreviousScreen),
                        progress::Message::InstallationFinished => {
                            self.install_in_flight = false;
                            self.reload_install_status();
                            Task::done(Message::UpdateTrayMenu)
                        }
                        _ => {
                            // A terminal failure comes through here as progress
                            // -1 (or an explicit error), unlike success which
                            // arrives as its own InstallationFinished message.
                            // Note it before `msg` is consumed so the colours in
                            // the picker are refreshed when the batch failed too.
                            let terminal = matches!(
                                &msg,
                                progress::Message::InstallationProgress(_, -1)
                                    | progress::Message::InstallationError(_)
                            );
                            let task = screen.update(msg).map(Message::ProgressScreen);
                            self.install_in_flight = screen.is_installing;
                            if terminal {
                                self.reload_install_status();
                            }
                            task
                        }
                    }
                } else {
                    Task::none()
                }
            }
            Message::RefreshAppNow { udid, app_path } => {
                if let Some(daemon_devices) = REFRESH_DAEMON_DEVICES.get() {
                    let daemon_devices = daemon_devices.clone();
                    let store_opt = self.account_store.clone();

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();

                        rt.block_on(async move {
                            if let Some(store) = store_opt {
                                if let Some(refresh_device) = store.get_refresh_device(&udid) {
                                    if let Some(app) = refresh_device
                                        .apps
                                        .iter()
                                        .find(|a| a.path.to_string_lossy() == app_path)
                                    {
                                        let start = std::time::Instant::now();
                                        let timeout = std::time::Duration::from_secs(60);

                                        log::info!(
                                            "Attempting to refresh app at {:?} on device {}, waiting...",
                                            app.path, udid
                                        );

                                        let device_opt = loop {
                                            if start.elapsed() > timeout {
                                                log::error!("Timeout waiting for device {}", udid);
                                                break None;
                                            }

                                            if let Ok(devices) = daemon_devices.lock() {
                                                if let Some(dev) = devices.get(&udid) {
                                                    break Some(dev.clone());
                                                }
                                            }
                                            tokio::time::sleep(std::time::Duration::from_secs(1))
                                                .await;
                                        };

                                        if let Some(device) = device_opt {
                                            let daemon = crate::refresh::RefreshDaemon::new();
                                            if let Err(e) = daemon
                                                .refresh_app(&store, refresh_device, app, &device)
                                                .await
                                            {
                                                log::error!(
                                                    "Failed to refresh app at {:?} on device {}: {}",
                                                    app.path,
                                                    udid,
                                                    e
                                                );
                                                notify_rust::Notification::new()
                                                    .summary("Impactor")
                                                    .body(&format!(
                                                        "Failed to refresh {} for {}: {}",
                                                        app.name.as_deref().unwrap_or("???"),
                                                        &refresh_device.name,
                                                        e
                                                    ))
                                                    .show()
                                                    .ok();
                                            }
                                        }
                                    }
                                }
                            }
                        });
                    });
                }
                Task::done(Message::UpdateTrayMenu)
            }
            Message::ForgetApp { udid, app_path } => {
                if let Some(store) = &mut self.account_store {
                    if let Some(mut refresh_device) = store.get_refresh_device(&udid).cloned() {
                        if let Some(app) = refresh_device
                            .apps
                            .iter()
                            .find(|a| a.path.to_string_lossy() == app_path)
                        {
                            let app_path_buf = app.path.clone();
                            std::thread::spawn(move || {
                                if app_path_buf.exists() {
                                    if let Err(e) = std::fs::remove_dir_all(&app_path_buf) {
                                        log::error!(
                                            "Failed to delete app at {:?}: {}",
                                            app_path_buf,
                                            e
                                        );
                                    } else {
                                        log::info!("Deleted app at {:?}", app_path_buf);
                                    }
                                }
                            });
                        }

                        refresh_device
                            .apps
                            .retain(|a| a.path.to_string_lossy() != app_path);

                        if refresh_device.apps.is_empty() {
                            let _ = store.remove_refresh_device_sync(&udid);
                        } else {
                            let _ = store.add_or_update_refresh_device_sync(refresh_device);
                        }

                        self.account_store = Some(Self::init_account_store_sync());
                    }
                }
                Task::done(Message::UpdateTrayMenu)
            }
            Message::UpdateTrayMenu => {
                self.account_store = Some(Self::init_account_store_sync());

                if let Some(store) = &self.account_store {
                    match &mut self.tray {
                        Some(existing_tray) => {
                            existing_tray.update_refresh_apps(&store);
                        }
                        None => {
                            let mut new_tray = ImpactorTray::new();
                            new_tray.update_refresh_apps(&store);
                            self.tray = Some(new_tray);
                        }
                    }
                }
                Task::none()
            }
            Message::StartInstallation => Task::none(),
            Message::MultiInstallToggleDevice(udid) => {
                if let Some(prompt) = &mut self.multi_install {
                    if !prompt.selected.remove(&udid) {
                        prompt.selected.insert(udid);
                    }
                }
                Task::none()
            }
            Message::MultiInstallSetAll(select_all) => {
                if let Some(prompt) = &mut self.multi_install {
                    prompt.selected.clear();
                    if select_all {
                        prompt
                            .selected
                            .extend(self.devices.iter().map(|device| device.udid.clone()));
                    }
                }
                Task::none()
            }
            Message::MultiInstallSelectUnfinished => {
                if let Some(prompt) = &mut self.multi_install {
                    // Replaces the selection rather than adding to it: the point
                    // of the button is "give me the ones still to do", so a
                    // device that already succeeded is dropped even if it was
                    // ticked by hand. Anything that is not a recorded success --
                    // failed, never installed, or a result old enough to have
                    // aged out -- counts as still to do.
                    let store = self.account_store.as_ref();
                    prompt.selected = self
                        .devices
                        .iter()
                        .filter(|device| {
                            store.and_then(|store| store.device_install_ok(&device.udid))
                                != Some(true)
                        })
                        .map(|device| device.udid.clone())
                        .collect();
                }
                Task::none()
            }
            Message::MultiInstallCancel => {
                self.multi_install = None;
                Task::none()
            }
            Message::BulkAddShow => {
                self.bulk_add = Some(BulkAddPrompt::default());
                Task::none()
            }
            Message::BulkAddEdit(action) => {
                if let Some(prompt) = &mut self.bulk_add {
                    prompt.content.perform(action);
                    prompt.error = None;
                }
                Task::none()
            }
            Message::BulkAddCancel => {
                self.bulk_add = None;
                Task::none()
            }
            Message::BulkAddConfirm => {
                let Some(prompt) = &mut self.bulk_add else {
                    return Task::none();
                };

                let (credentials, malformed) = parse_bulk_credentials(&prompt.content.text());

                if credentials.is_empty() {
                    // Keep the box open with what they pasted still in it.
                    prompt.error = Some(if malformed.is_empty() {
                        rust_i18n::t!("bulk_add_empty").to_string()
                    } else {
                        rust_i18n::t!(
                            "bulk_add_no_valid_lines",
                            count = malformed.len().to_string()
                        )
                        .to_string()
                    });
                    return Task::none();
                }

                // Every account on the list is signed in, including ones that
                // are already stored. A repeat sign-in overwrites the stored
                // entry with fresh tokens and today's added date, which is how
                // a list is used to mark a batch of accounts as "new" again so
                // auto-pick reaches for them first. (Existing accounts used to
                // be skipped here.)
                let mut queue = BulkAddQueue::default();
                queue.total = credentials.len();
                queue.gap = self
                    .account_store
                    .as_ref()
                    .map(|store| std::time::Duration::from_secs(store.network().bulk_gap_secs))
                    .unwrap_or(BULK_LOGIN_GAP);
                queue.pending.extend(credentials);

                queue.failed.extend(
                    malformed
                        .into_iter()
                        .map(|line| rust_i18n::t!("bulk_add_bad_line", line = line).to_string()),
                );

                self.bulk_add = None;
                self.bulk_queue = Some(queue);

                Task::done(Message::BulkAddNext)
            }
            Message::BulkAddNext => {
                let Some(queue) = &mut self.bulk_queue else {
                    return Task::none();
                };

                let next = if queue.cancelled {
                    None
                } else {
                    queue.pending.pop_front()
                };

                let Some(credential) = next else {
                    let summary = queue.summary();
                    self.bulk_queue = None;
                    self.bulk_waiting = false;
                    self.account_store = Some(Self::init_account_store_sync());

                    if let ImpactorScreen::Settings(_) = self.current_screen {
                        self.current_screen =
                            ImpactorScreen::Settings(settings::SettingsScreen::new());
                    }

                    // Off the UI thread: rfd's dialog blocks until dismissed.
                    std::thread::spawn(move || {
                        rfd::MessageDialog::new()
                            .set_title(&rust_i18n::t!("bulk_add_title").to_string())
                            .set_description(&summary)
                            .set_buttons(rfd::MessageButtons::Ok)
                            .show();
                    });

                    return Task::none();
                };

                // Position in the run: everything popped so far, this one
                // included. Not `failed.len()`, which also counts malformed
                // lines that were never queued and would start the header part
                // way through the list.
                // A re-queued account makes the list one longer than it was.
                let to_attempt = queue.total + queue.requeued.len();
                let attempted = to_attempt - queue.pending.len();
                queue.current = Some(credential.email.clone());
                queue.current_credential = Some(credential.clone());
                let gap = queue.gap;

                let open = Message::BulkAddOpen(credential, attempted, to_attempt);

                if attempted == 1 {
                    return Task::done(open);
                }

                // Space the sign-ins out. Sleeping on a thread and waking the
                // task through a oneshot keeps the UI responsive and avoids
                // needing a tokio runtime on the executor.
                self.bulk_waiting = true;

                let (ready_tx, ready_rx) = iced::futures::channel::oneshot::channel::<()>();
                std::thread::spawn(move || {
                    std::thread::sleep(gap);
                    let _ = ready_tx.send(());
                });

                Task::perform(async move { ready_rx.await.ok() }, move |_| open.clone())
            }
            Message::BulkAddOpen(credential, index, total) => {
                self.bulk_waiting = false;

                if self.bulk_queue.is_none() {
                    return Task::none();
                }

                let (login_window, task) = login_window::LoginWindow::new_for_queue(
                    credential.email,
                    credential.password,
                    (index, total),
                );
                let id = login_window.window_id().unwrap();
                self.login_windows.insert(id, login_window);

                task.map(move |msg| Message::LoginWindowMessage(id, msg))
            }
            Message::MultiInstallConfirm => {
                let Some(prompt) = self.multi_install.as_ref() else {
                    return Task::none();
                };

                let targets: Vec<Device> = self
                    .devices
                    .iter()
                    .filter(|device| prompt.selected.contains(&device.udid))
                    .cloned()
                    .collect();

                if targets.is_empty() {
                    return Task::none();
                }

                // Signing with an Apple ID needs an account for every target. The
                // store already falls back to the ticked account, so a gap here
                // means no account is signed in at all.
                let needs_login = match &self.current_screen {
                    ImpactorScreen::Installer(installer) => {
                        matches!(installer.options.mode, plume_utils::SignerMode::Pem)
                            && targets.iter().any(|device| {
                                self.account_store
                                    .as_ref()
                                    .and_then(|store| {
                                        store.account_for_device(Some(device.udid.as_str()))
                                    })
                                    .is_none()
                            })
                    }
                    _ => false,
                };

                if needs_login {
                    // The picker stays open so the run resumes once they are in.
                    self.pending_installation = true;

                    let (login_window, task) = login_window::LoginWindow::new();
                    let id = login_window.window_id().unwrap();
                    self.login_windows.insert(id, login_window);
                    return task.map(move |msg| Message::LoginWindowMessage(id, msg));
                }

                self.multi_install = None;
                self.start_install_batch(targets.into_iter().map(Some).collect())
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let device_subscription = subscriptions::device_listener();

        let tray_subscription = subscriptions::tray_subscription();

        let hover_subscription = if let ImpactorScreen::Main(_) = self.current_screen {
            subscriptions::file_hover_subscription()
        } else {
            Subscription::none()
        };

        let progress_subscription =
            if let ImpactorScreen::Progress(ref progress) = self.current_screen {
                subscriptions::installation_progress_listener(progress.progress_rx.clone()).map(
                    |(status, progress_val)| {
                        Message::ProgressScreen(progress::Message::InstallationProgress(
                            status,
                            progress_val,
                        ))
                    },
                )
            } else {
                Subscription::none()
            };

        let tray_menu_refresh_subscription = subscriptions::tray_menu_refresh_subscription();
        let certificate_reset_subscription = subscriptions::certificate_reset_subscription();
        let relaunch_subscription = subscriptions::relaunch_subscription();

        let close_subscription = iced::event::listen_with(|event, _status, _id| {
            if let iced::Event::Window(window::Event::CloseRequested) = event {
                return Some(Message::HideWindow);
            }
            None
        });

        // Only keys that reached the application unclaimed: a Delete typed
        // into the proxy box is consumed by the text input first and never
        // arrives here, so it cannot remove an account.
        let hotkey_subscription = iced::event::listen_with(|event, status, id| {
            use iced::keyboard::{Event as KeyEvent, Key, key::Named};

            if status != iced::event::Status::Ignored {
                return None;
            }
            let iced::Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            let hotkey = match key {
                Key::Named(Named::Delete) => HotKey::Delete,
                Key::Named(Named::Tab) if modifiers.shift() => HotKey::ShiftTab,
                Key::Named(Named::Tab) => HotKey::Tab,
                _ => return None,
            };
            Some(Message::HotKey(hotkey, id))
        });

        Subscription::batch(vec![
            device_subscription,
            tray_subscription,
            hover_subscription,
            progress_subscription,
            tray_menu_refresh_subscription,
            certificate_reset_subscription,
            relaunch_subscription,
            close_subscription,
            hotkey_subscription,
        ])
    }

    pub fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        if let Some(login_window) = self.login_windows.get(&window_id) {
            return login_window
                .view()
                .map(move |msg| Message::LoginWindowMessage(window_id, msg));
        }

        let has_device = self.selected_device.is_some();
        let screen_content = self.view_current_screen(has_device);
        let top_bar = self.view_top_bar();
        let base: Element<'_, Message> =
            container(column(vec![top_bar, screen_content]).spacing(appearance::THEME_PADDING))
                .padding(appearance::THEME_PADDING)
                // Fill the window so an overlaid dialog gets the whole area to
                // centre itself in; otherwise a short screen clips it.
                .width(Fill)
                .height(Fill)
                .into();

        if self.certificate_reset_queue.front().is_some() {
            stack![base, self.view_certificate_reset_prompt()].into()
        } else if self.multi_install.is_some() {
            stack![base, self.view_multi_install_prompt()].into()
        } else if self.bulk_waiting {
            stack![base, self.view_bulk_waiting()].into()
        } else if self.bulk_add.is_some() {
            stack![base, self.view_bulk_add_prompt()].into()
        } else {
            base
        }
    }

    fn view_current_screen(&self, has_device: bool) -> Element<'_, Message> {
        match &self.current_screen {
            ImpactorScreen::Main(screen) => screen.view().map(Message::MainScreen),
            ImpactorScreen::Utilities(screen) => screen.view().map(Message::UtilitiesScreen),
            ImpactorScreen::Settings(screen) => screen
                .view(&self.account_store)
                .map(Message::SettingsScreen),
            ImpactorScreen::Installer(screen) => {
                screen.view(has_device).map(Message::InstallerScreen)
            }
            ImpactorScreen::Progress(screen) => screen.view().map(Message::ProgressScreen),
        }
    }

    fn view_top_bar(&self) -> Element<'_, Message> {
        let device_names: Vec<String> = self.devices.iter().map(|d| d.to_string()).collect();
        let selected_device_name = self.selected_device.as_ref().map(|d| d.to_string());
        let placeholder_str = selected_device_name
            .as_ref()
            .map(String::as_str)
            .unwrap_or("No Device");

        let right_button = if matches!(self.current_screen, ImpactorScreen::Settings(_)) {
            button(appearance::icon(appearance::CHEVRON_BACK))
                .on_press(Message::PreviousScreen)
                .style(appearance::s_button)
        } else if matches!(self.current_screen, ImpactorScreen::Utilities(_)) {
            button(appearance::icon(appearance::CHEVRON_BACK))
                .on_press(Message::PreviousScreen)
                .style(appearance::s_button)
        } else {
            button(appearance::icon(appearance::GEAR))
                .style(appearance::s_button)
                .on_press(Message::NavigateToScreen(ImpactorScreenType::Settings))
        };

        container(
            row![
                // The label carries name, OS version and UDID now, which does not
                // fit in the old 250px box parked on the right. Let the picker take
                // the whole bar instead, so it lines up with the content below it.
                pick_list(
                    device_names,
                    selected_device_name.clone(),
                    Message::ComboBoxSelected
                )
                .style(appearance::s_pick_list)
                .placeholder(placeholder_str)
                .width(Fill),
                right_button
            ]
            .align_y(iced::Alignment::Center)
            .spacing(appearance::THEME_PADDING),
        )
        .width(Fill)
        .into()
    }

    /// Shown between sign-ins, when no login window is up and the app would
    /// otherwise look frozen.
    fn view_bulk_waiting(&self) -> Element<'_, Message> {
        let dialog = container(
            column![
                text(rust_i18n::t!("bulk_add_waiting")).size(appearance::THEME_FONT_SIZE + 2.0),
                text(rust_i18n::t!(
                    "bulk_add_waiting_why",
                    seconds = self
                        .bulk_queue
                        .as_ref()
                        .map(|queue| queue.gap)
                        .unwrap_or(BULK_LOGIN_GAP)
                        .as_secs()
                        .to_string()
                ))
                .size(appearance::THEME_FONT_SIZE - 2.0),
            ]
            .spacing(appearance::THEME_PADDING),
        )
        .padding(appearance::THEME_PADDING * 2.0)
        .max_width(420.0)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme.palette().background)),
            border: iced::Border {
                width: 1.0,
                color: theme.palette().primary,
                radius: appearance::THEME_CORNER_RADIUS.into(),
            },
            ..Default::default()
        });

        container(dialog)
            .width(Fill)
            .height(Fill)
            .center(Fill)
            .style(|_theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.45,
                })),
                ..Default::default()
            })
            .into()
    }

    fn view_bulk_add_prompt(&self) -> Element<'_, Message> {
        let Some(prompt) = &self.bulk_add else {
            return container(text("")).into();
        };

        let editor = text_editor(&prompt.content)
            .placeholder(rust_i18n::t!("bulk_add_placeholder"))
            .on_action(Message::BulkAddEdit)
            .height(160.0);

        let mut body = column![
            text(rust_i18n::t!("bulk_add_title")).size(appearance::THEME_FONT_SIZE + 2.0),
            text(rust_i18n::t!("bulk_add_hint")).size(appearance::THEME_FONT_SIZE - 2.0),
            editor,
        ]
        .spacing(appearance::THEME_PADDING);

        if let Some(error) = &prompt.error {
            body = body.push(text(error).style(|_theme| iced::widget::text::Style {
                color: Some(iced::Color::from_rgb(1.0, 0.3, 0.3)),
            }));
        }

        body = body.push(
            row![
                container(text("")).width(Fill),
                button(text(rust_i18n::t!("cancel")))
                    .on_press(Message::BulkAddCancel)
                    .style(appearance::s_button),
                button(text(rust_i18n::t!("bulk_add_start")))
                    .on_press(Message::BulkAddConfirm)
                    .style(appearance::p_button),
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(iced::Alignment::Center),
        );

        let dialog = container(body)
            .padding(appearance::THEME_PADDING * 2.0)
            .max_width(500.0)
            .style(|theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(theme.palette().background)),
                border: iced::Border {
                    width: 1.0,
                    color: theme.palette().primary,
                    radius: appearance::THEME_CORNER_RADIUS.into(),
                },
                ..Default::default()
            });

        container(dialog)
            .width(Fill)
            .height(Fill)
            .center(Fill)
            .style(|_theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.45,
                })),
                ..Default::default()
            })
            .into()
    }

    fn view_multi_install_prompt(&self) -> Element<'_, Message> {
        let Some(prompt) = &self.multi_install else {
            return container(text("")).into();
        };

        // Only worth showing the resolved account when it can differ per device.
        let show_accounts = self
            .account_store
            .as_ref()
            .is_some_and(|store| store.auto_select_account());

        // Which account each ticked device would actually get, assigned in the
        // order the run will install them -- so the picker shows the real spread
        // (newest account fills three devices, then the next) rather than the
        // same top pick under every row.
        let projected_accounts: std::collections::HashMap<String, String> = if show_accounts {
            let selected_in_order: Vec<String> = self
                .devices
                .iter()
                .filter(|device| prompt.selected.contains(&device.udid))
                .map(|device| device.udid.clone())
                .collect();

            self.account_store
                .as_ref()
                .map(|store| store.project_device_accounts(&selected_in_order))
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };

        const OK_COLOR: iced::Color = iced::Color {
            r: 0.16,
            g: 0.65,
            b: 0.27,
            a: 1.0,
        };
        const FAIL_COLOR: iced::Color = iced::Color {
            r: 0.85,
            g: 0.25,
            b: 0.25,
            a: 1.0,
        };

        // How a device's last install went: `Some(true)` done, `Some(false)`
        // failed, `None` never installed (or the record aged out). Read from the
        // store, which was written to disk as the run went, so this survives the
        // app being reopened or crashing mid-run.
        let install_ok = |udid: &str| -> Option<bool> {
            self.account_store
                .as_ref()
                .and_then(|store| store.device_install_ok(udid))
        };

        let status_color = |udid: &str| -> Option<iced::Color> {
            install_ok(udid).map(|ok| if ok { OK_COLOR } else { FAIL_COLOR })
        };

        // Why the last attempt failed, when one did and a reason was saved.
        // Shown whether or not the device is ticked: the point of it is to be
        // readable before deciding what to retry.
        let install_error = |udid: &str| -> Option<String> {
            self.account_store
                .as_ref()
                .and_then(|store| store.device_install_error(udid))
                .map(str::to_string)
        };

        let entries = self.devices.iter().fold(
            column![].spacing(appearance::THEME_PADDING),
            |list, device| {
                let udid = device.udid.clone();
                let checked = prompt.selected.contains(&udid);
                let color = status_color(&device.udid);

                let mut check = checkbox(checked)
                    .label(device.to_string())
                    .on_toggle(move |_| Message::MultiInstallToggleDevice(udid.clone()));

                // Colour the device name by its last install result, keeping the
                // rest of the checkbox exactly as the theme draws it.
                if let Some(color) = color {
                    check = check.style(move |theme, status| {
                        let mut style = iced::widget::checkbox::primary(theme, status);
                        style.text_color = Some(color);
                        style
                    });
                }

                let mut entry = column![check].spacing(2);

                // Only a ticked device gets an account line, and it shows the
                // account this device will actually be signed with once the run
                // assigns them in order -- not the same top pick for all.
                if show_accounts && checked {
                    let account = projected_accounts.get(&device.udid).cloned().unwrap_or_else(
                        || rust_i18n::t!("multi_install_no_account").to_string(),
                    );

                    entry = entry.push(text(account).size(appearance::THEME_FONT_SIZE - 2.0));
                }

                // The saved reason for a red row, so the run does not have to be
                // repeated to find out what went wrong -- and so it is still
                // there after the app is closed and reopened.
                if let Some(reason) = install_error(&device.udid) {
                    entry = entry.push(
                        text(rust_i18n::t!("multi_install_last_error", reason = reason).to_string())
                            .size(appearance::THEME_FONT_SIZE - 2.0)
                            .color(FAIL_COLOR),
                    );
                }

                list.push(entry)
            },
        );

        let count = prompt.selected.len();
        let all_selected = !self.devices.is_empty()
            && self
                .devices
                .iter()
                .all(|device| prompt.selected.contains(&device.udid));

        let toggle_all_label = if all_selected {
            rust_i18n::t!("multi_install_clear")
        } else {
            rust_i18n::t!("multi_install_all")
        };

        // Everything that is not a recorded success: the red rows and the ones
        // with no colour at all. Disabled when there are none, which is itself
        // the answer to "is anything left to do?".
        let unfinished = self
            .devices
            .iter()
            .filter(|device| install_ok(&device.udid) != Some(true))
            .count();

        // Two rows rather than one: four buttons of text do not fit across the
        // dialog, and picking what to install is a separate step from starting.
        let select_actions = row![
            button(text(toggle_all_label))
                .on_press(Message::MultiInstallSetAll(!all_selected))
                .style(appearance::s_button),
            button(text(
                rust_i18n::t!(
                    "multi_install_select_unfinished",
                    count = unfinished.to_string()
                )
                .to_string()
            ))
            .on_press_maybe((unfinished > 0).then_some(Message::MultiInstallSelectUnfinished))
            .style(appearance::s_button),
            container(text("")).width(Fill),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(iced::Alignment::Center);

        let actions = row![
            container(text("")).width(Fill),
            button(text(rust_i18n::t!("back")))
                .on_press(Message::MultiInstallCancel)
                .style(appearance::s_button),
            button(text(
                rust_i18n::t!("multi_install_start", count = count.to_string()).to_string()
            ))
            .on_press_maybe((count > 0).then_some(Message::MultiInstallConfirm))
            .style(appearance::p_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(iced::Alignment::Center);

        let body: Element<'_, Message> = if self.devices.is_empty() {
            text(rust_i18n::t!("multi_install_no_devices")).into()
        } else {
            container(scrollable(entries)).max_height(220.0).into()
        };

        let dialog = container(
            column![
                text(rust_i18n::t!("multi_install")).size(appearance::THEME_FONT_SIZE + 2.0),
                body,
                select_actions,
                actions,
            ]
            .spacing(appearance::THEME_PADDING),
        )
        .padding(appearance::THEME_PADDING * 2.0)
        .max_width(500.0)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme.palette().background)),
            border: iced::Border {
                width: 1.0,
                color: theme.palette().primary,
                radius: appearance::THEME_CORNER_RADIUS.into(),
            },
            ..Default::default()
        });

        container(dialog)
            .width(Fill)
            .height(Fill)
            .center(Fill)
            .style(|_theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.45,
                })),
                ..Default::default()
            })
            .into()
    }

    fn view_certificate_reset_prompt(&self) -> Element<'_, Message> {
        let Some(request) = self.certificate_reset_queue.front() else {
            return container(text("")).into();
        };

        let actions = row![
            button(text("Cancel"))
                .on_press(Message::CancelCertificateReset)
                .style(appearance::s_button),
            button(text("Continue"))
                .on_press(Message::ConfirmCertificateReset)
                .style(appearance::p_button),
        ]
        .spacing(appearance::THEME_PADDING);

        let dialog = container(
            column![
                text("Certificate reset required").size(appearance::THEME_FONT_SIZE + 2.0),
                text(&request.message),
                actions,
            ]
            .spacing(appearance::THEME_PADDING),
        )
        .padding(appearance::THEME_PADDING * 2.0)
        .max_width(420.0)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme.palette().background)),
            border: iced::Border {
                width: 1.0,
                color: theme.palette().warning,
                radius: appearance::THEME_CORNER_RADIUS.into(),
            },
            ..Default::default()
        });

        container(dialog)
            .width(Fill)
            .height(Fill)
            .center(Fill)
            .style(|_theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.45,
                })),
                ..Default::default()
            })
            .into()
    }

    fn navigate_to_screen(&mut self, screen_type: ImpactorScreenType) {
        match screen_type {
            ImpactorScreenType::Main => {
                if let ImpactorScreen::Installer(installer) = &self.current_screen {
                    if let Some(package) = installer.selected_package.clone() {
                        package.remove_package_stage();
                    }
                }

                // Only safe once the worker thread is done with the staged copy;
                // while an install is running we would rather leak a temp folder
                // than delete the files being signed out from under it.
                if !self.install_in_flight {
                    if let Some(package) = self
                        .parked_installer
                        .take()
                        .and_then(|installer| installer.selected_package)
                    {
                        package.remove_package_stage();
                    }
                }

                self.current_screen = ImpactorScreen::Main(general::GeneralScreen::new());
            }
            ImpactorScreenType::Utilities => {
                self.current_screen = ImpactorScreen::Utilities(utilties::UtilitiesScreen::new(
                    self.selected_device.clone(),
                ));
            }
            ImpactorScreenType::Settings => {
                self.current_screen = ImpactorScreen::Settings(settings::SettingsScreen::new());
            }
            ImpactorScreenType::Progress => {
                self.current_screen = ImpactorScreen::Progress(progress::ProgressScreen::new());
            }
            _ => {}
        }
    }

    /// The account an install would use right now: the one remembered for the
    /// selected device when auto-select is on, otherwise the one ticked in Settings.
    fn account_for_selected_device(&self) -> Option<&plume_store::GsaAccount> {
        let udid = self.selected_device.as_ref().map(|d| d.udid.as_str());
        self.account_store
            .as_ref()
            .and_then(|store| store.account_for_device(udid))
    }

    fn start_installation_task(&mut self) -> Task<Message> {
        let target = self.selected_device.clone();
        self.start_install_batch(vec![target])
    }

    /// Hands the saved proxy and 429 wait to every Apple-facing client.
    fn apply_network(network: &plume_store::NetworkSettings) {
        plume_core::net::configure(plume_core::net::Settings {
            proxy: network.proxy_url.clone(),
            rate_limit_wait: std::time::Duration::from_secs(network.rate_limit_wait_secs),
        });
    }

    /// Reloads the account store from disk once a run has ended.
    ///
    /// The batch writes each device's result straight to `accounts.json` as it
    /// goes; the copy held in memory here does not see those writes, so it is
    /// re-read now. That is what lets the multi-install picker show the fresh
    /// green/red colours the next time it is opened.
    fn reload_install_status(&mut self) {
        self.account_store = Some(Self::init_account_store_sync());
    }

    /// Runs the loaded package against each target in turn on a worker thread.
    ///
    /// A `None` target is the export path, which has no device at all. Every
    /// target keeps its own account, resolved inside the batch, so a run can span
    /// devices provisioned by different Apple IDs.
    fn start_install_batch(&mut self, targets: Vec<Option<Device>>) -> Task<Message> {
        let ImpactorScreen::Installer(installer) = &self.current_screen else {
            return Task::none();
        };

        let Some(package) = installer.selected_package.clone() else {
            return Task::none();
        };

        let options = installer.options.clone();
        let targets = if targets.is_empty() {
            vec![None]
        } else {
            targets
        };
        let count = targets.len();
        let mut store = self.account_store.clone().unwrap_or_default();

        let (tx, rx) = std::sync::mpsc::channel();
        let progress_rx = std::sync::Arc::new(std::sync::Mutex::new(rx));

        let mut progress_screen = progress::ProgressScreen::new();
        progress_screen.start_installation(progress_rx.clone());

        if let ImpactorScreen::Installer(installer) = std::mem::replace(
            &mut self.current_screen,
            ImpactorScreen::Progress(progress_screen),
        ) {
            self.parked_installer = Some(installer);
        }
        self.install_in_flight = true;

        // A panic anywhere in the install (a failed unwrap deep in signing, the
        // device layer, or a dependency) unwinds out of the worker. Caught here
        // it becomes a normal failure the progress screen can show, instead of a
        // stuck bar -- and, because the profile now unwinds rather than aborts,
        // it no longer takes the whole app down. crash.log still records it.
        let tx_panic = tx.clone();
        std::thread::spawn(move || {
            let tx_error = tx.clone();
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    // The staged copy is deliberately left in place. It is what lets the
                    // user go back and install the same package to another device, and it
                    // is cleaned up when they return to the drop zone.
                    match subscriptions::run_install_batch(
                        &package, &targets, &options, &mut store, &tx,
                    )
                    .await
                    {
                        Ok(()) => {
                            let message = if count > 1 {
                                rust_i18n::t!("multi_install_done", count = count.to_string())
                                    .to_string()
                            } else {
                                "Installation complete!".to_string()
                            };
                            let _ = tx.send((message, 100));
                        }
                        Err(e) => {
                            let _ = tx_error.send((format!("Error: {}", e), -1));
                        }
                    }
                });
            }));

            if ran.is_err() {
                let _ = tx_panic.send((
                    rust_i18n::t!("install_crashed").to_string(),
                    -1,
                ));
            }
        });

        Task::none()
    }
}
