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

/// How long to wait between sign-ins during a bulk run.
///
/// Signing several Apple IDs in from one machine back to back looks like abuse
/// to Apple: the sessions get invalidated and the accounts can be locked, which
/// then needs a manual unlock on appleid.apple.com. Spacing the attempts out
/// costs a couple of minutes and avoids that.
const BULK_LOGIN_GAP: std::time::Duration = std::time::Duration::from_secs(30);

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
    current: Option<String>,
    added: Vec<String>,
    failed: Vec<String>,
    skipped: Vec<String>,
    cancelled: bool,
}

impl BulkAddQueue {
    /// What to show once the run stops, either finished or cancelled.
    fn summary(&self) -> String {
        let mut lines = vec![rust_i18n::t!(
            "bulk_add_summary_counts",
            added = self.added.len().to_string(),
            skipped = self.skipped.len().to_string(),
            failed = self.failed.len().to_string()
        )
        .to_string()];

        if !self.skipped.is_empty() {
            lines.push(String::new());
            lines.push(rust_i18n::t!("bulk_add_summary_skipped").to_string());
            lines.extend(self.skipped.iter().map(|email| format!("  {email}")));
        }

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
    selected_locale: Option<String>,
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
        let selected_locale = store.locale().map(str::to_string);
        if let Some(code) = &selected_locale {
            rust_i18n::set_locale(code);
        }
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
                selected_locale,
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
                                    queue.added.push(email);
                                }
                                true
                            }
                            login_window::Message::LoginFailed(error) => {
                                let error = error.clone();
                                if let Some(queue) = &mut self.bulk_queue {
                                    if let Some(email) = queue.current.take() {
                                        queue.failed.push(format!("{email}: {error}"));
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
                        settings::Message::SelectLocale(choice) => {
                            self.selected_locale = choice.clone();
                            let effective = choice
                                .clone()
                                .or_else(|| current_locale::current_locale().ok())
                                .unwrap_or_else(|| "en".to_string());
                            rust_i18n::set_locale(&effective);
                            if let Some(store) = &mut self.account_store {
                                if let Err(err) = store.set_locale_sync(choice) {
                                    log::error!("Failed to persist locale: {err}");
                                }
                            }
                            Task::none()
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
                            Task::done(Message::UpdateTrayMenu)
                        }
                        _ => {
                            let task = screen.update(msg).map(Message::ProgressScreen);
                            self.install_in_flight = screen.is_installing;
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

                // Accounts already signed in are left alone rather than being
                // signed in again, which would cost a round trip per account and
                // reset nothing useful.
                let existing: std::collections::HashSet<String> = self
                    .account_store
                    .as_ref()
                    .map(|store| {
                        store
                            .accounts()
                            .keys()
                            .map(|email| email.to_ascii_lowercase())
                            .collect()
                    })
                    .unwrap_or_default();

                let mut queue = BulkAddQueue::default();

                for credential in credentials {
                    if existing.contains(&credential.email.to_ascii_lowercase()) {
                        queue.skipped.push(credential.email);
                    } else {
                        queue.pending.push_back(credential);
                    }
                }

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

                // Count only the accounts actually being signed in. `total`
                // includes the ones skipped as already present, so subtracting
                // from it would start the count part way through the list.
                let attempted = queue.added.len() + queue.failed.len() + 1;
                let to_attempt = attempted + queue.pending.len();
                queue.current = Some(credential.email.clone());

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
                    std::thread::sleep(BULK_LOGIN_GAP);
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

        Subscription::batch(vec![
            device_subscription,
            tray_subscription,
            hover_subscription,
            progress_subscription,
            tray_menu_refresh_subscription,
            certificate_reset_subscription,
            relaunch_subscription,
            close_subscription,
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
                .view(
                    &self.account_store,
                    &self.selected_locale,
                    self.selected_device.as_ref(),
                )
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
                    seconds = BULK_LOGIN_GAP.as_secs().to_string()
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

        let entries = self.devices.iter().fold(
            column![].spacing(appearance::THEME_PADDING),
            |list, device| {
                let udid = device.udid.clone();
                let checked = prompt.selected.contains(&udid);

                let mut entry = column![
                    checkbox(checked)
                        .label(device.to_string())
                        .on_toggle(move |_| Message::MultiInstallToggleDevice(udid.clone()))
                ]
                .spacing(2);

                if show_accounts {
                    let account = self
                        .account_store
                        .as_ref()
                        .and_then(|store| store.account_for_device(Some(device.udid.as_str())))
                        .map(|account| account.email().clone())
                        .unwrap_or_else(|| rust_i18n::t!("multi_install_no_account").to_string());

                    entry = entry.push(text(account).size(appearance::THEME_FONT_SIZE - 2.0));
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

        let actions = row![
            button(text(toggle_all_label))
                .on_press(Message::MultiInstallSetAll(!all_selected))
                .style(appearance::s_button),
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

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let tx_error = tx.clone();
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
        });

        Task::none()
    }
}
