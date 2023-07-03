mod subscriptions;

use cosmic::cosmic_config::{config_subscription, Config, CosmicConfigEntry};
use cosmic::iced::wayland::popup::{destroy_popup, get_popup};
use cosmic::iced::{
    widget::{button, column, row, text, Row, Space},
    window, Alignment, Application, Color, Command, Length, Subscription,
};
use cosmic::iced_core::image;
use cosmic::iced_widget::button::StyleSheet;
use cosmic_applet::{applet_button_theme, CosmicAppletHelper};

use cosmic::iced_style::application::{self, Appearance};

use cosmic::iced_widget::{horizontal_space, scrollable, Column};
use cosmic::theme::{Button, Svg};
use cosmic::widget::{divider, icon};
use cosmic::Renderer;
use cosmic::{Element, Theme};
use cosmic_notifications_config::NotificationsConfig;
use cosmic_notifications_util::{AppletEvent, Notification};
use cosmic_time::{anim, chain, id, once_cell::sync::Lazy, Instant, Timeline};
use std::borrow::Cow;
use std::process;
use tokio::sync::mpsc::Sender;
use tracing::info;

#[tokio::main(flavor = "current_thread")]
pub async fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();

    info!("Notifications applet");

    let helper = CosmicAppletHelper::default();
    Notifications::run(helper.window_settings())
}

static DO_NOT_DISTURB: Lazy<id::Toggler> = Lazy::new(id::Toggler::unique);

#[derive(Default)]
struct Notifications {
    applet_helper: CosmicAppletHelper,
    theme: Theme,
    config: NotificationsConfig,
    config_helper: Option<Config>,
    icon_name: String,
    popup: Option<window::Id>,
    id_ctr: u128,
    notifications: Vec<Notification>,
    timeline: Timeline,
    dbus_sender: Option<Sender<subscriptions::dbus::Input>>,
}

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    DoNotDisturb(chain::Toggler, bool),
    Settings,
    Ignore,
    Frame(Instant),
    Theme(Theme),
    NotificationEvent(AppletEvent),
    Config(NotificationsConfig),
    DbusEvent(subscriptions::dbus::Output),
    Dismissed(u32),
}

impl Application for Notifications {
    type Message = Message;
    type Theme = Theme;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    fn new(_flags: ()) -> (Notifications, Command<Message>) {
        let applet_helper = CosmicAppletHelper::default();
        let theme = applet_helper.theme();
        let helper = Config::new(
            cosmic_notifications_config::ID,
            NotificationsConfig::version(),
        )
        .ok();

        let config: NotificationsConfig = helper
            .as_ref()
            .map(|helper| {
                NotificationsConfig::get_entry(helper).unwrap_or_else(|(errors, config)| {
                    for err in errors {
                        tracing::error!("{:?}", err);
                    }
                    config
                })
            })
            .unwrap_or_default();
        (
            Notifications {
                applet_helper,
                theme,
                icon_name: "notification-alert-symbolic".to_string(),
                config_helper: helper,
                config,
                ..Default::default()
            },
            Command::none(),
        )
    }

    fn title(&self) -> String {
        String::from("Notifications")
    }

    fn theme(&self) -> Theme {
        self.theme.clone()
    }

    fn close_requested(&self, _id: window::Id) -> Self::Message {
        Message::Ignore
    }

    fn style(&self) -> <Self::Theme as application::StyleSheet>::Style {
        <Self::Theme as application::StyleSheet>::Style::Custom(Box::new(|theme| Appearance {
            background_color: Color::from_rgba(0.0, 0.0, 0.0, 0.0),
            text_color: theme.cosmic().on_bg_color().into(),
        }))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            self.applet_helper.theme_subscription(0).map(Message::Theme),
            config_subscription::<u64, NotificationsConfig>(
                0,
                cosmic_notifications_config::ID.into(),
                NotificationsConfig::version(),
            )
            .map(|(_, res)| match res {
                Ok(config) => Message::Config(config),
                Err((errors, config)) => {
                    for err in errors {
                        tracing::error!("{:?}", err);
                    }
                    Message::Config(config)
                }
            }),
            self.timeline
                .as_subscription()
                .map(|(_, now)| Message::Frame(now)),
            subscriptions::dbus::proxy().map(Message::DbusEvent),
            subscriptions::notifications::notifications().map(Message::NotificationEvent),
        ])
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Theme(t) => {
                self.theme = t;
                Command::none()
            }
            Message::Frame(now) => {
                self.timeline.now(now);
                Command::none()
            }
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    self.id_ctr += 1;
                    let new_id = window::Id(self.id_ctr);
                    self.popup.replace(new_id);

                    let popup_settings = self.applet_helper.get_popup_settings(
                        window::Id(0),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    get_popup(popup_settings)
                }
            }
            Message::DoNotDisturb(chain, b) => {
                self.timeline.set_chain(chain).start();
                self.config.do_not_disturb = b;
                if let Some(helper) = &self.config_helper {
                    if let Err(err) = self.config.write_entry(helper) {
                        tracing::error!("{:?}", err);
                    }
                }
                Command::none()
            }
            Message::Settings => {
                let _ = process::Command::new("cosmic-settings notifications").spawn();
                Command::none()
            }
            Message::NotificationEvent(e) => {
                dbg!(e);
                Command::none()
            }
            Message::Ignore => Command::none(),
            Message::Config(config) => {
                self.config = config;
                Command::none()
            }
            Message::Dismissed(id) => {
                self.notifications.retain(|n| n.id != id);
                if let Some(tx) = &self.dbus_sender {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Err(err) = tx.send(subscriptions::dbus::Input::Dismiss(id)).await {
                            tracing::error!("{:?}", err);
                        }
                    });
                }
                Command::none()
            }
            Message::DbusEvent(e) => match e {
                subscriptions::dbus::Output::Ready(tx) => {
                    self.dbus_sender.replace(tx);
                    Command::none()
                }
            },
        }
    }

    fn view(&self, id: window::Id) -> Element<Message> {
        if id == window::Id(0) {
            self.applet_helper
                .icon_button(&self.icon_name)
                .on_press(Message::TogglePopup)
                .into()
        } else {
            let do_not_disturb = row![anim!(
                DO_NOT_DISTURB,
                &self.timeline,
                String::from("Do Not Disturb"),
                self.config.do_not_disturb,
                Message::DoNotDisturb
            )
            .width(Length::Fill)]
            .padding([0, 24]);

            let settings =
                row_button(vec!["Notification Settings...".into()]).on_press(Message::Settings);

            let notifications = if self.notifications.len() == 0 {
                row![
                    Space::with_width(Length::Fill),
                    column![text_icon(&self.icon_name, 40), "No Notifications"]
                        .align_items(Alignment::Center),
                    Space::with_width(Length::Fill)
                ]
                .spacing(12)
            } else {
                let mut notifs = Vec::with_capacity(self.notifications.len());

                for n in &self.notifications {
                    let summary = text(if n.summary.len() > 24 {
                        Cow::from(format!(
                            "{:.26}...",
                            n.summary.lines().next().unwrap_or_default()
                        ))
                    } else {
                        Cow::from(&n.summary)
                    })
                    .size(18);
                    let urgency = n.urgency();

                    notifs.push(
                        cosmic::widget::button(Button::Custom {
                            active: Box::new(move |t| {
                                let style = if urgency > 1 {
                                    Button::Primary
                                } else {
                                    Button::Secondary
                                };
                                let cosmic = t.cosmic();
                                let mut a = t.active(&style);
                                a.border_radius = 8.0.into();
                                a.background = Some(Color::from(cosmic.bg_color()).into());
                                a.border_color = Color::from(cosmic.bg_divider());
                                a.border_width = 1.0;
                                a
                            }),
                            hover: Box::new(move |t| {
                                let style = if urgency > 1 {
                                    Button::Primary
                                } else {
                                    Button::Secondary
                                };
                                let cosmic = t.cosmic();
                                let mut a = t.hovered(&style);
                                a.border_radius = 8.0.into();
                                a.background = Some(Color::from(cosmic.bg_color()).into());
                                a.border_color = Color::from(cosmic.bg_divider());
                                a.border_width = 1.0;
                                a
                            }),
                        })
                        .custom(vec![column!(
                            match n.image() {
                                Some(cosmic_notifications_util::Image::File(path)) => {
                                    row![icon(path.as_path(), 32), summary]
                                        .spacing(8)
                                        .align_items(Alignment::Center)
                                }
                                Some(cosmic_notifications_util::Image::Name(name)) => {
                                    row![icon(name.as_str(), 32), summary]
                                        .spacing(8)
                                        .align_items(Alignment::Center)
                                }
                                Some(cosmic_notifications_util::Image::Data {
                                    width,
                                    height,
                                    data,
                                }) => {
                                    let handle =
                                        image::Handle::from_pixels(*width, *height, data.clone());
                                    row![icon(handle, 32), summary]
                                        .spacing(8)
                                        .align_items(Alignment::Center)
                                }
                                None => row![summary],
                            },
                            text(if n.body.len() > 38 {
                                Cow::from(format!(
                                    "{:.40}...",
                                    n.body.lines().next().unwrap_or_default()
                                ))
                            } else {
                                Cow::from(&n.summary)
                            })
                            .size(14),
                            horizontal_space(Length::Fixed(300.0)),
                        )
                        .spacing(8)
                        .into()])
                        .on_press(Message::Dismissed(n.id))
                        .into(),
                    );
                }

                row!(scrollable(
                    Column::with_children(notifs)
                        .spacing(8)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                )
                .width(Length::Shrink)
                .height(Length::Fixed(400.0)))
                .width(Length::Shrink)
            };

            let main_content = column![
                divider::horizontal::light(),
                notifications,
                divider::horizontal::light()
            ]
            .padding([0, 24])
            .spacing(12);

            let content = column![]
                .align_items(Alignment::Start)
                .spacing(12)
                .padding([12, 0])
                .push(do_not_disturb)
                .push(main_content)
                .push(settings);

            self.applet_helper.popup_container(content).into()
        }
    }
}

// todo put into libcosmic doing so will fix the row_button's boarder radius
fn row_button(
    mut content: Vec<Element<Message>>,
) -> cosmic::iced::widget::Button<Message, Renderer> {
    content.insert(0, Space::with_width(Length::Fixed(24.0)).into());
    content.push(Space::with_width(Length::Fixed(24.0)).into());

    button(
        Row::with_children(content)
            .spacing(4)
            .align_items(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(36.0))
    .style(applet_button_theme())
}

fn text_icon(name: &str, size: u16) -> cosmic::widget::Icon {
    icon(name, size).style(Svg::Symbolic)
}
