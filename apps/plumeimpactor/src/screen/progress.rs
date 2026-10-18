use std::sync::{Arc, Mutex, mpsc};

use iced::Element;
use iced::Length::Fill;
use iced::Task;
use iced::widget::{button, column, container, progress_bar, row, scrollable, text};
use rust_i18n::t;

use crate::appearance;
use crate::subscriptions::lanes;

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

type ProgressReceiver = Arc<Mutex<mpsc::Receiver<(String, i32)>>>;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    InstallationProgress(String, i32),
    InstallationError(String),
    InstallationFinished,
    Back,
}

#[derive(Debug, Clone)]
pub struct ProgressScreen {
    pub status: String,
    pub progress: i32,
    pub is_installing: bool,
    pub progress_rx: Option<ProgressReceiver>,
}

impl ProgressScreen {
    pub fn new() -> Self {
        Self {
            status: "Idle.".to_string(),
            progress: 0,
            is_installing: false,
            progress_rx: None,
        }
    }

    pub fn start_installation(&mut self, rx: ProgressReceiver) {
        self.is_installing = true;
        self.progress = 0;
        self.status = "Idle.".to_string();
        self.progress_rx = Some(rx);
        // Called before the worker starts, so this cannot wipe lanes the new
        // run has already set up -- only whatever the previous run left.
        lanes::clear();
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InstallationProgress(status, progress) => {
                self.status = status.clone();
                self.progress = progress;

                if progress == -1 {
                    self.progress_rx = None;
                    self.is_installing = false;

                    let error_msg = status.clone();
                    std::thread::spawn(move || {
                        rfd::MessageDialog::new()
                            .set_title(t!("progress_failed"))
                            .set_description(&error_msg)
                            .set_buttons(rfd::MessageButtons::Ok)
                            .show();
                    });
                } else if progress >= 100 {
                    self.progress_rx = None;
                    self.is_installing = false;

                    return Task::done(Message::InstallationFinished);
                }

                Task::none()
            }
            Message::InstallationError(error) => {
                self.progress = -1;
                self.status = format!("ERR: {}", error);
                self.progress_rx = None;
                self.is_installing = false;

                std::thread::spawn(move || {
                    rfd::MessageDialog::new()
                        .set_title(t!("progress_failed"))
                        .set_description(&error)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                });

                Task::none()
            }
            Message::InstallationFinished => {
                self.progress = 100;
                self.status = t!("progress_finished").to_string();
                self.progress_rx = None;
                self.is_installing = false;

                Task::none()
            }
            Message::Back => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let overall_bar = progress_bar(0.0..=100.0, self.progress as f32);

        let screen_content = column![
            text(t!("progress_installing_application")).size(14),
            text(format!("{}% – {}", self.progress, self.status)).size(14),
            overall_bar,
            self.view_lanes(),
        ]
        .spacing(appearance::THEME_PADDING);

        column![
            container(screen_content).width(Fill).height(Fill),
            self.view_buttons()
        ]
        .into()
    }

    /// One bar per lane -- per Apple ID working at the same time -- with the
    /// devices it carries listed under it, each with what it is doing or, once
    /// it has failed, why.
    ///
    /// Read fresh from the shared board on every draw; the worker sends a
    /// progress message after each change, and that message is what triggers
    /// the redraw.
    fn view_lanes(&self) -> Element<'_, Message> {
        let board = lanes::snapshot();
        if board.devices.is_empty() {
            return container(text("")).height(Fill).into();
        }

        let count = |state: lanes::State| {
            board
                .devices
                .iter()
                .filter(|device| device.state == state)
                .count()
        };
        let summary = t!(
            "progress_lanes_summary",
            done = count(lanes::State::Done).to_string(),
            failed = count(lanes::State::Failed).to_string(),
            running = count(lanes::State::Working).to_string(),
            waiting = count(lanes::State::Waiting).to_string()
        )
        .to_string();

        let small = appearance::THEME_FONT_SIZE;

        let tint = |state: lanes::State| match state {
            lanes::State::Done => Some(OK_COLOR),
            lanes::State::Failed => Some(FAIL_COLOR),
            _ => None,
        };

        let device_row = |device: &lanes::DeviceLine| -> Element<'static, Message> {
            let mut note = text(device.note.clone()).size(small).width(Fill);
            if let Some(color) = tint(device.state) {
                note = note.color(color);
            }
            row![
                text(format!("\u{2022} {}", device.name))
                    .size(small)
                    .width(180),
                note,
            ]
            .spacing(8)
            .padding(iced::Padding::ZERO.left(14))
            .into()
        };

        let mut rows = column![].spacing(4);

        for (index, lane) in board.lanes.iter().enumerate() {
            let bar_style: fn(&iced::Theme) -> progress_bar::Style = match lane.state {
                lanes::State::Done => progress_bar::success,
                lanes::State::Failed => progress_bar::danger,
                lanes::State::Working => progress_bar::primary,
                lanes::State::Waiting => progress_bar::secondary,
            };

            let mut status = text(lane.status.clone()).size(small);
            if let Some(color) = tint(lane.state) {
                status = status.color(color);
            }

            let header = row![
                text(lane.title.clone()).size(small + 1.0).width(Fill),
                progress_bar(0.0..=100.0, lane.percent as f32)
                    .length(160)
                    .girth(8)
                    .style(bar_style),
                text(format!("{}%", lane.percent)).size(small).width(34),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);

            let mut block = column![header, status].spacing(2);
            for device in board.devices.iter().filter(|device| device.lane == Some(index)) {
                block = block.push(device_row(device));
            }

            rows = rows.push(container(block).padding([4, 0]));
        }

        // Devices between lanes: planned for none yet, or sent back to find
        // another Apple ID.
        let unplaced: Vec<&lanes::DeviceLine> = board
            .devices
            .iter()
            .filter(|device| device.lane.is_none())
            .collect();
        if !unplaced.is_empty() {
            rows = rows.push(text(t!("progress_lanes_waiting")).size(small + 1.0));
            for device in unplaced {
                rows = rows.push(device_row(device));
            }
        }

        column![
            text(summary).size(small),
            scrollable(rows.padding(iced::Padding::ZERO.right(14))).height(Fill),
        ]
        .spacing(6)
        .height(Fill)
        .into()
    }

    fn view_buttons(&self) -> Element<'_, Message> {
        container(row![
            button(appearance::icon_text(
                appearance::CHEVRON_BACK,
                t!("back"),
                None
            ))
            .on_press_maybe((!self.is_installing).then_some(Message::Back))
            .width(Fill)
            .style(appearance::s_button)
        ])
        .width(Fill)
        .into()
    }
}

impl Default for ProgressScreen {
    fn default() -> Self {
        Self::new()
    }
}
