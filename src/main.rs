// Launching the release build without a console window.
// Debug builds keep the console so logs and panics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;
mod model;
mod taskbar;
mod util;

use app::RgFtpApp;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);

            let mut options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.0), px(820.0)), cx)),
                window_min_size: Some(size(px(1280.0), px(820.0))),
                ..TitleBar::window_options()
            };
            if let Some(titlebar) = &mut options.titlebar {
                titlebar.title = Some("RGFTP - SFTP 客户端".into());
            }

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| RgFtpApp::new(window, cx))
            })
            .expect("failed to open RGFTP window");
        });
}
