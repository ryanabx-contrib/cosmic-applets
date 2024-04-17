mod app;
use localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();

    app::run()
}
