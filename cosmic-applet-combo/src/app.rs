

struct CosmicAppletCombo {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
}


#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    CloseRequested(window::Id),
}


impl cosmic::Application for CosmicAppletCombo {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = config::APP_ID;

    fn init(
        core: cosmic::app::Core,
        _flags: Self::Flags,
    ) -> (Self, iced::Command<cosmic::app::Message<Self::Message>>) {
        (
            Self {
                core,
                icon_name: "bluetooth-symbolic".to_string(),
                token_tx: None,
                ..Default::default()
            },
            Command::none(),
        )
    }
}

