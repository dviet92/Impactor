use iced::widget::{button, column, container, image, row, text};
use iced::{Center, Color, Element, Fill, Task};
use plume_utils::Package;
use rust_i18n::t;

use crate::appearance;
use std::sync::OnceLock;

const INSTALL_IMAGE: &[u8] = include_bytes!("./general.png");
const INSTALL_IMAGE_HEIGHT: f32 = 130.0;
const GITHUB_URL: &str = "https://github.com/claration/Impactor";
const DONATE_URL: &str = "https://github.com/sponsors/claration";

#[derive(Debug, Clone)]
pub enum Message {
    FilesHovered,
    FilesHoveredLeft,
    FilesDropped(Vec<std::path::PathBuf>),
    OpenFileDialog,
    FileSelected(Option<std::path::PathBuf>),
    NavigateToInstaller(plume_utils::Package),
    NavigateToUtilities,
    OpenGitHub,
    OpenDonate,
}

/// Says why a package could not be opened.
///
/// Dropping the error on the floor made a rejected file look like a drop the
/// window had ignored, which sent people looking at the app when the error
/// already named the problem -- a file still syncing, or a disk with no room
/// left to stage it.
fn report_package_error(error: plume_utils::Error) {
    let description = error.to_string();

    log::warn!("Failed to open package: {description}");

    // Off the UI thread: rfd's dialog blocks until it is dismissed.
    std::thread::spawn(move || {
        rfd::MessageDialog::new()
            .set_title(&t!("package_open_failed").to_string())
            .set_description(&description)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    });
}

#[derive(Debug, Clone, Default)]
pub struct GeneralScreen;

impl GeneralScreen {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenFileDialog => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter(t!("ipa"), &["ipa", "tipa"])
                            .set_title(t!("select_ipa"))
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::FileSelected,
                );
            }
            Message::FileSelected(path) => {
                if let Some(path) = path {
                    match Package::new(path) {
                        Ok(package) => {
                            return Task::done(Message::NavigateToInstaller(package));
                        }
                        Err(e) => report_package_error(e),
                    }
                }
                Task::none()
            }
            Message::FilesDropped(paths) => {
                for path in paths {
                    let is_package = path
                        .extension()
                        .is_some_and(|ext| ext == "ipa" || ext == "tipa");

                    if !is_package {
                        continue;
                    }

                    match Package::new(path) {
                        Ok(package) => {
                            return Task::done(Message::NavigateToInstaller(package));
                        }
                        // Stop at the first package we tried. Falling through to
                        // the next dropped file would leave the message that
                        // explains the failure buried under a second one.
                        Err(e) => {
                            report_package_error(e);
                            break;
                        }
                    }
                }
                Task::none()
            }
            Message::OpenGitHub => {
                let _ = open::that(GITHUB_URL);
                Task::none()
            }
            Message::OpenDonate => {
                let _ = open::that(DONATE_URL);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        static INSTALL_IMAGE_HANDLE: OnceLock<image::Handle> = OnceLock::new();
        let image_handle =
            INSTALL_IMAGE_HANDLE.get_or_init(|| image::Handle::from_bytes(INSTALL_IMAGE));

        let screen_content = column![
            container(text("")).height(appearance::THEME_PADDING * 2.0),
            image(image_handle.clone()).height(INSTALL_IMAGE_HEIGHT),
            text(t!("drag_and_drop"))
                .size(appearance::THEME_FONT_SIZE + 7.0)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.3))
        ]
        .spacing(10)
        .align_x(Center);

        let footer_links = row![
            button(appearance::icon_text(
                appearance::STAR,
                t!("star_us"),
                Some(Color::from_rgb(1.0, 0.75, 0.8)),
            ))
            .on_press(Message::OpenGitHub)
            .style(iced::widget::button::text),
            button(appearance::icon_text(
                appearance::STAR,
                t!("donate"),
                Some(Color::from_rgb(1.0, 0.75, 0.8)),
            ))
            .on_press(Message::OpenDonate)
            .style(iced::widget::button::text)
        ];

        column![
            container(screen_content).center(Fill).height(Fill),
            container(footer_links).width(Fill),
            self.view_buttons()
        ]
        .into()
    }

    fn view_buttons(&self) -> Element<'_, Message> {
        container(
            row![
                button(appearance::icon_text(
                    appearance::WRENCH,
                    t!("utilities"),
                    None
                ))
                .on_press(Message::NavigateToUtilities)
                .width(Fill)
                .style(appearance::s_button),
                button(appearance::icon_text(
                    appearance::DOWNLOAD,
                    t!("import_ipa"),
                    None
                ))
                .on_press(Message::OpenFileDialog)
                .width(Fill)
                .style(appearance::s_button)
            ]
            .spacing(appearance::THEME_PADDING),
        )
        .width(Fill)
        .into()
    }
}
