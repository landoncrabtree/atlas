//! Atlas — application binary.

use anyhow::Result;
use gpui::{
    div, prelude::*, px, rgb, App, Application, Bounds, Context, Render, TitlebarOptions, Window,
    WindowBounds, WindowKind, WindowOptions,
};
use tracing_subscriber::EnvFilter;

struct HelloAtlas;

impl Render for HelloAtlas {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .items_center()
            .justify_center()
            .bg(rgb(0x0e1116))
            .text_color(rgb(0xe6edf3))
            .child(div().text_size(px(28.0)).child("Atlas"))
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgb(0x7d8590))
                    .child("a file explorer — pre-alpha"),
            )
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,atlas=debug")),
        )
        .init();

    tracing::info!("starting atlas");

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, gpui::size(px(1100.0), px(720.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Atlas".into()),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
            }),
            kind: WindowKind::Normal,
            ..Default::default()
        };

        cx.open_window(options, |_window, cx| cx.new(|_| HelloAtlas))
            .expect("failed to open atlas window");

        cx.activate(true);
    });

    Ok(())
}
