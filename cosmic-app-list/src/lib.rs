// SPDX-License-Identifier: MPL-2.0-only
mod app;
mod buffer;
mod capture;
mod config;
mod dmabuf;
mod localize;
mod screencopy;
mod wayland_handler;

use localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();

    app::run()
}
