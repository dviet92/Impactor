use std::collections::HashMap;

use iced::widget::{
    Text, button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Fill, Task};
use plume_store::{AccountStore, NetworkSettings};
use rust_i18n::t;

use crate::appearance;

/// Row colours for the three-device cap, shown only while auto-pick is on.
///
/// Green: the account can still take a device. Red: it is carrying its three
/// and the oldest of them is not yet seven days old, so auto-pick will skip it
/// until one ages out.
///
/// Two pairs because the two button styles invert each other. An unselected row
/// is light text on the dark background, so its colours have to be bright; the
/// selected row is drawn on the primary pink with dark text, where the same
/// bright green would disappear.
const CAP_FREE: iced::Color = iced::Color {
    r: 0.35,
    g: 0.82,
    b: 0.45,
    a: 1.0,
};
const CAP_FULL: iced::Color = iced::Color {
    r: 0.95,
    g: 0.45,
    b: 0.45,
    a: 1.0,
};
const CAP_FREE_ON_PRIMARY: iced::Color = iced::Color {
    r: 0.04,
    g: 0.28,
    b: 0.10,
    a: 1.0,
};
const CAP_FULL_ON_PRIMARY: iced::Color = iced::Color {
    r: 0.46,
    g: 0.03,
    b: 0.03,
    a: 1.0,
};

/// A row cell at the list's text size, tinted when there is a colour to apply
/// and left to the theme when there is not.
fn cell(value: String, color: Option<iced::Color>) -> Text<'static> {
    let cell = text(value).size(appearance::THEME_FONT_SIZE);

    match color {
        Some(color) => cell.color(color),
        None => cell,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub name: String,
    pub id: String,
}

impl std::fmt::Display for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.id)
    }
}

/// The two pages of Settings. Accounts is what is used day to day; Connection
/// holds the proxy and the waits, which are set once and otherwise stay out of
/// the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Accounts,
    Connection,
}

#[derive(Debug, Clone)]
pub enum Message {
    ShowLogin,
    ShowBulkAdd,
    SelectAccount(usize),
    RemoveAccount(usize),
    /// Mark the selected account as full for the week, or take the mark off.
    ToggleAccountFullMark(usize),
    /// Remove whichever account is ticked; the Delete key.
    RemoveSelected,
    ExportP12,
    SelectTeam(String, String),
    FetchTeams(String),
    TeamsLoaded(String, Vec<Team>),
    ToggleAutoStart(bool),
    ToggleAutoAccount(bool),
    ToggleGroupIdentities(bool),
    IdentityGroupSizeEdited(String),
    SelectTab(Tab),
    // Connection tab. Edits are kept as drafts on the screen and validated
    // live; the parent persists and applies them.
    ProxyEdited(String),
    ProxyApply,
    ProxyClear,
    ProxyTest,
    ProxyTested(Result<String, String>),
    BulkGapEdited(String),
    RateLimitWaitEdited(String),
    InstallParallelEdited(String),
}

#[derive(Debug)]
pub struct SettingsScreen {
    teams: HashMap<String, Vec<Team>>,
    loading_teams: Option<String>,
    tab: Tab,
    /// What is in the proxy box while it differs from what is saved.
    proxy_draft: Option<String>,
    /// Why the draft cannot be applied, shown under the box.
    proxy_error: Option<String>,
    proxy_testing: bool,
    proxy_test_result: Option<Result<String, String>>,
    bulk_gap_draft: Option<String>,
    rate_limit_wait_draft: Option<String>,
    install_parallel_draft: Option<String>,
    identity_group_size_draft: Option<String>,
}

impl SettingsScreen {
    pub fn new() -> Self {
        Self {
            teams: HashMap::new(),
            loading_teams: None,
            tab: Tab::default(),
            proxy_draft: None,
            proxy_error: None,
            proxy_testing: false,
            proxy_test_result: None,
            bulk_gap_draft: None,
            rate_limit_wait_draft: None,
            install_parallel_draft: None,
            identity_group_size_draft: None,
        }
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The proxy URL as typed, trimmed; `None` when the box is empty.
    pub fn proxy_draft(&self) -> Option<&str> {
        self.proxy_draft
            .as_deref()
            .map(str::trim)
            .filter(|draft| !draft.is_empty())
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FetchTeams(ref email) => {
                self.loading_teams = Some(email.clone());
                Task::none()
            }
            Message::TeamsLoaded(email, teams) => {
                self.teams.insert(email, teams);
                self.loading_teams = None;
                Task::none()
            }
            Message::SelectTab(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::ProxyEdited(draft) => {
                self.proxy_test_result = None;
                let trimmed = draft.trim();
                self.proxy_error = if trimmed.is_empty() {
                    None
                } else {
                    plume_core::net::validate_proxy(trimmed).err()
                };
                self.proxy_draft = Some(draft);
                Task::none()
            }
            // The parent has saved the draft, or refused it and the error
            // stays under the box; when saved, the box mirrors the store again.
            Message::ProxyApply => {
                if self.proxy_error.is_none() {
                    self.proxy_draft = None;
                }
                Task::none()
            }
            Message::ProxyClear => {
                self.proxy_draft = None;
                self.proxy_error = None;
                self.proxy_test_result = None;
                Task::none()
            }
            Message::ProxyTest => {
                self.proxy_testing = true;
                self.proxy_test_result = None;
                Task::none()
            }
            Message::ProxyTested(result) => {
                self.proxy_testing = false;
                self.proxy_test_result = Some(result);
                Task::none()
            }
            Message::BulkGapEdited(draft) => {
                self.bulk_gap_draft = Some(draft);
                Task::none()
            }
            Message::RateLimitWaitEdited(draft) => {
                self.rate_limit_wait_draft = Some(draft);
                Task::none()
            }
            Message::InstallParallelEdited(draft) => {
                self.install_parallel_draft = Some(draft);
                Task::none()
            }
            Message::IdentityGroupSizeEdited(draft) => {
                self.identity_group_size_draft = Some(draft);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn view<'a>(&'a self, account_store: &'a Option<AccountStore>) -> Element<'a, Message> {
        let Some(store) = account_store else {
            return column![text(t!("settings_loading_accounts"))]
                .spacing(appearance::THEME_PADDING)
                .padding(appearance::THEME_PADDING)
                .into();
        };

        let page = match self.tab {
            Tab::Accounts => self.view_accounts(store),
            Tab::Connection => self.view_connection(store.network()),
        };

        column![self.view_tabs(), page]
            .spacing(appearance::THEME_PADDING)
            .into()
    }

    fn view_tabs(&self) -> Element<'_, Message> {
        let tab_button = |label: String, tab: Tab| {
            let style = if self.tab == tab {
                appearance::p_button
            } else {
                appearance::s_button
            };
            button(text(label).size(appearance::THEME_FONT_SIZE))
                .on_press(Message::SelectTab(tab))
                .style(style)
                .width(Fill)
        };

        row![
            tab_button(t!("settings_tab_accounts").to_string(), Tab::Accounts),
            tab_button(t!("settings_tab_connection").to_string(), Tab::Connection),
        ]
        .spacing(appearance::THEME_PADDING)
        .into()
    }

    fn view_accounts<'a>(&'a self, store: &'a AccountStore) -> Element<'a, Message> {
        let mut accounts: Vec<_> = store.accounts().iter().collect();
        accounts.sort_by_key(|(email, _)| *email);

        let selected_index = store
            .selected_account()
            .and_then(|acc| accounts.iter().position(|(e, _)| *e == acc.email()));

        // One reading of the clock for the whole list, so two rows can never
        // disagree about which sign-ins are still inside the window.
        let now = chrono::Utc::now();

        let mut content = column![].spacing(appearance::THEME_PADDING);

        // Bare checkboxes: what each one does is in the doc comments and the
        // project notes, not on screen under every tick.
        content = content.push(
            checkbox(store.auto_select_account())
                .label(t!("settings_auto_account"))
                .on_toggle(Message::ToggleAutoAccount),
        );
        content = content.push(
            checkbox(store.group_identities())
                .label(t!("settings_group_identities"))
                .on_toggle(Message::ToggleGroupIdentities),
        );

        // Only worth a line on screen while grouping is on: with it off every
        // sign-in uses the machine identity and the rate means nothing.
        if store.group_identities() {
            let size = self
                .identity_group_size_draft
                .clone()
                .unwrap_or_else(|| store.identity_group_size().to_string());

            content = content.push(
                row![
                    text(
                        t!(
                            "settings_identity_group_size",
                            hours = plume_store::IDENTITY_GROUP_WINDOW_HOURS
                        )
                        .to_string()
                    )
                    .size(appearance::THEME_FONT_SIZE)
                    .width(Fill),
                    text_input("5", &size)
                        .on_input(Message::IdentityGroupSizeEdited)
                        .size(appearance::THEME_FONT_SIZE)
                        .width(60.0),
                    text(t!("settings_apple_ids").to_string()).size(appearance::THEME_FONT_SIZE),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }

        if !accounts.is_empty() {
            let account_list = accounts.iter().enumerate().fold(
                column![].spacing(appearance::THEME_PADDING),
                |content, (index, (email, account))| {
                    let selected = Some(index) == selected_index;
                    let marker = if selected { "[✓] " } else { "[ ] " };
                    let style = if selected {
                        appearance::p_button
                    } else {
                        appearance::s_button
                    };

                    // Only auto-pick cares about the cap, so only auto-pick
                    // colours the row by it. Red is "full for now", not "bad":
                    // the account is fine, it is just out of slots until its
                    // oldest install turns seven days old.
                    let cap_color = store.auto_select_account().then(|| {
                        match (store.remaining_device_slots(email) > 0, selected) {
                            (true, false) => CAP_FREE,
                            (false, false) => CAP_FULL,
                            (true, true) => CAP_FREE_ON_PRIMARY,
                            (false, true) => CAP_FULL_ON_PRIMARY,
                        }
                    });

                    // Counters, kept to a glance rather than a sentence. With
                    // auto-select on: devices still free of the three-device
                    // cap. With grouping on: which shared identity signed this
                    // account in (numbered from 1 for people, stored from 0)
                    // and how many sign-ins that identity has spent in the
                    // current window.
                    let mut status = String::new();
                    if store.auto_select_account() {
                        status.push_str(&t!(
                            "settings_account_slots_left",
                            left = store.remaining_device_slots(email),
                            max = plume_store::AUTO_ACCOUNT_MAX_DEVICES
                        ));
                    }
                    // A mark is otherwise only visible as a full row, which
                    // says nothing about why -- and says nothing at all with
                    // auto-pick off.
                    if store.account_is_marked_full(email) {
                        if !status.is_empty() {
                            status.push(' ');
                        }
                        status.push_str(&t!("settings_marked_full"));
                    }
                    if let Some(group) = account.identity_group() {
                        if !status.is_empty() {
                            status.push(' ');
                        }
                        status.push_str(&t!(
                            "settings_identity_group",
                            group = group + 1,
                            used = store.identity_group_recent_sign_ins(group, now),
                            max = store.identity_group_size()
                        ));
                    }

                    // The day this account last signed a device. The counters
                    // above only look back seven days, so an account showing
                    // three free slots is either untouched or simply idle
                    // since then -- this column is what tells them apart.
                    let last_install = account
                        .last_install_at()
                        .map(|at| at.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "-".to_string());

                    let account_button = button(
                        row![
                            cell(format!("{}{}", marker, account.email()), cap_color)
                                .width(Fill)
                                .align_x(Alignment::Start),
                            cell(status, cap_color),
                            cell(last_install, cap_color)
                                .width(84.0)
                                .align_x(Alignment::End),
                        ]
                        .spacing(appearance::THEME_PADDING)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::SelectAccount(index))
                    .style(style)
                    .width(Fill);

                    let mut account_row = row![account_button].spacing(appearance::THEME_PADDING);

                    if selected {
                        let team_id = account.team_id();
                        let is_loading = self.loading_teams.as_ref() == Some(email);
                        let teams = self.teams.get(*email).cloned().unwrap_or_default();

                        let current_team = if !team_id.is_empty() {
                            teams.iter().find(|t| t.id == *team_id).cloned()
                        } else {
                            None
                        };

                        let placeholder = if is_loading {
                            t!("settings_select_teams").to_string()
                        } else if !team_id.is_empty() {
                            team_id.to_string()
                        } else {
                            t!("settings_loading_teams").to_string()
                        };

                        let email_owned = email.to_string();

                        let team_pick = pick_list(teams, current_team, move |selected: Team| {
                            Message::SelectTeam(email_owned.clone(), selected.id)
                        })
                        .placeholder(placeholder)
                        .on_open(Message::FetchTeams(email.to_string()))
                        .style(appearance::s_pick_list)
                        .width(Fill);

                        account_row = account_row.push(team_pick);
                    }

                    content.push(account_row)
                },
            );

            content = content.push(container(scrollable(account_list)).height(Fill).style(
                |theme: &iced::Theme| container::Style {
                    border: iced::Border {
                        width: 1.0,
                        color: theme.palette().background.scale_alpha(0.5),
                        radius: appearance::THEME_CORNER_RADIUS.into(),
                    },
                    ..Default::default()
                },
            ));
        } else {
            content = content.push(text(t!("settings_no_accounts_yet")).height(Fill));
        }

        let auto_start_enabled = crate::startup::auto_start_enabled();
        content = content.push(self.view_auto_start_toggle(auto_start_enabled));
        let selected_marked = store
            .selected_account()
            .is_some_and(|account| store.account_is_marked_full(account.email()));

        content = content.push(self.view_account_buttons(selected_index, selected_marked));

        content.into()
    }

    /// Proxy and the two waits. Everything here is about how Apple is reached,
    /// which is why it is kept off the accounts page: with a proxy pool in
    /// front, the waits can go to zero, and someone who has set that up once
    /// does not want to look at it again.
    fn view_connection<'a>(&'a self, network: &'a NetworkSettings) -> Element<'a, Message> {
        let saved_proxy = network.proxy_url.as_deref().unwrap_or("");
        let shown_proxy = self.proxy_draft.as_deref().unwrap_or(saved_proxy);
        let dirty = self.proxy_draft.is_some() && shown_proxy.trim() != saved_proxy;
        let can_apply = dirty && self.proxy_error.is_none();

        let proxy_input = text_input(&t!("settings_proxy_placeholder"), shown_proxy)
            .on_input(Message::ProxyEdited)
            .on_submit(Message::ProxyApply)
            .size(appearance::THEME_FONT_SIZE)
            .width(Fill);

        let apply = button(text(t!("settings_proxy_apply")).size(appearance::THEME_FONT_SIZE))
            .on_press_maybe(can_apply.then_some(Message::ProxyApply))
            .style(appearance::p_button);
        let clear = button(text(t!("settings_proxy_clear")).size(appearance::THEME_FONT_SIZE))
            .on_press_maybe(
                (!saved_proxy.is_empty() || !shown_proxy.is_empty())
                    .then_some(Message::ProxyClear),
            )
            .style(appearance::s_button);
        let test = button(text(t!("settings_proxy_test")).size(appearance::THEME_FONT_SIZE))
            .on_press_maybe((!self.proxy_testing).then_some(Message::ProxyTest))
            .style(appearance::s_button);

        let mut proxy_block = column![
            text(t!("settings_proxy")).size(appearance::THEME_FONT_SIZE + 1.0),
            text(t!("settings_proxy_hint")).size(appearance::THEME_FONT_SIZE - 2.0),
            proxy_input,
            row![apply, clear, test].spacing(appearance::THEME_PADDING),
        ]
        .spacing(appearance::THEME_PADDING);

        // Status line: the validation error while typing, the test outcome,
        // or what is in effect.
        let status = if let Some(error) = &self.proxy_error {
            t!("settings_proxy_invalid", error = error.as_str()).to_string()
        } else if self.proxy_testing {
            t!("settings_proxy_testing").to_string()
        } else if let Some(result) = &self.proxy_test_result {
            match result {
                Ok(detail) => t!("settings_proxy_test_ok", detail = detail.as_str()).to_string(),
                Err(error) => {
                    t!("settings_proxy_test_failed", error = error.as_str()).to_string()
                }
            }
        } else if saved_proxy.is_empty() {
            t!("settings_proxy_direct").to_string()
        } else {
            t!("settings_proxy_active", proxy = redact_proxy(saved_proxy).as_str()).to_string()
        };
        proxy_block = proxy_block.push(text(status).size(appearance::THEME_FONT_SIZE - 2.0));

        let bulk_gap = self
            .bulk_gap_draft
            .clone()
            .unwrap_or_else(|| network.bulk_gap_secs.to_string());
        let rate_wait = self
            .rate_limit_wait_draft
            .clone()
            .unwrap_or_else(|| network.rate_limit_wait_secs.to_string());

        let parallel = self
            .install_parallel_draft
            .clone()
            .unwrap_or_else(|| network.install_parallel.to_string());

        let number_field =
            |label: String, value: String, unit: String, on_input: fn(String) -> Message| {
                row![
                    text(label).width(Fill),
                    text_input("0", &value)
                        .on_input(on_input)
                        .size(appearance::THEME_FONT_SIZE)
                        .width(90.0),
                    text(unit),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center)
            };

        let waits_block = column![
            text(t!("settings_waits")).size(appearance::THEME_FONT_SIZE + 1.0),
            text(t!("settings_waits_hint")).size(appearance::THEME_FONT_SIZE - 2.0),
            number_field(
                t!("settings_bulk_gap").to_string(),
                bulk_gap,
                t!("settings_seconds").to_string(),
                Message::BulkGapEdited
            ),
            number_field(
                t!("settings_rate_limit_wait").to_string(),
                rate_wait,
                t!("settings_seconds").to_string(),
                Message::RateLimitWaitEdited
            ),
        ]
        .spacing(appearance::THEME_PADDING);

        let install_block = column![
            text(t!("settings_installing")).size(appearance::THEME_FONT_SIZE + 1.0),
            text(t!("settings_install_hint")).size(appearance::THEME_FONT_SIZE - 2.0),
            number_field(
                t!("settings_install_parallel").to_string(),
                parallel,
                t!("settings_devices").to_string(),
                Message::InstallParallelEdited
            ),
        ]
        .spacing(appearance::THEME_PADDING);

        scrollable(
            column![proxy_block, waits_block, install_block]
                .spacing(appearance::THEME_PADDING * 2.0)
                .padding(iced::Padding::ZERO.right(appearance::THEME_PADDING)),
        )
        .height(Fill)
        .into()
    }

    fn view_auto_start_toggle(&self, auto_start_enabled: bool) -> Element<'_, Message> {
        checkbox(auto_start_enabled)
            .label(t!("settings_launch_on_startup"))
            .on_toggle(Message::ToggleAutoStart)
            .into()
    }

    /// The account actions on one row. Everything that needs an account stays
    /// visible but disabled until one is ticked, so the row never jumps.
    ///
    /// `selected_marked` decides what the mark button says: an account already
    /// marked full offers to have the mark taken off instead.
    fn view_account_buttons(
        &self,
        selected_index: Option<usize>,
        selected_marked: bool,
    ) -> Element<'_, Message> {
        let action = |icon: &'static str, label: String, message: Option<Message>| {
            button(appearance::icon_text(icon, label.into(), None))
                .on_press_maybe(message)
                .style(appearance::s_button)
                .width(Fill)
        };

        row![
            action(
                appearance::PLUS,
                t!("settings_add_account").to_string(),
                Some(Message::ShowLogin)
            ),
            action(
                appearance::FILE,
                t!("settings_add_accounts_from_list").to_string(),
                Some(Message::ShowBulkAdd)
            ),
            action(
                appearance::MINUS,
                t!("settings_remove_account").to_string(),
                selected_index.map(Message::RemoveAccount)
            ),
            action(
                appearance::SHARE,
                t!("settings_export_p12").to_string(),
                selected_index.map(|_| Message::ExportP12)
            ),
            action(
                appearance::STAR,
                if selected_marked {
                    t!("settings_unmark_full").to_string()
                } else {
                    t!("settings_mark_full").to_string()
                },
                selected_index.map(Message::ToggleAccountFullMark)
            ),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Alignment::Center)
        .into()
    }
}

/// A proxy URL safe to show on screen: `user:pass@` becomes `***@`.
fn redact_proxy(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(scheme_end), Some(at)) if at > scheme_end => {
            format!("{}***@{}", &url[..scheme_end + 3], &url[at + 1..])
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::redact_proxy;

    #[test]
    fn credentials_are_hidden_but_host_is_kept() {
        assert_eq!(
            redact_proxy("socks5://user:p@ss@10.0.0.1:1080"),
            "socks5://***@10.0.0.1:1080"
        );
        assert_eq!(redact_proxy("http://proxy:8080"), "http://proxy:8080");
    }
}
