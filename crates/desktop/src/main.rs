//! Lotus desktop application.
//!
//! This is the smallest possible GPUI shell: one window with an instance
//! list and a placeholder for the two 3DS screens. The worker, the debugger
//! views and the trace viewer are wired in later; the point of this spike is
//! to prove that the pinned GPUI revision builds and opens a window on the
//! host platform.

use gpui::{
    App, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb,
    size,
};
use gpui_platform::application;
use lotus_desktop::{LotusUiState, SessionSummary};

/// Root view of the Lotus window.
struct LotusWindow {
    state: LotusUiState,
}

impl Render for LotusWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(40.0))
            .px_4()
            .border_b_1()
            .border_color(rgb(0x3a3a3a))
            .child("Lotus")
            .child(format!("worker protocol {}", self.state.protocol_version));

        let instances = if self.state.sessions.is_empty() {
            div()
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(rgb(0x9a9a9a))
                .child("No emulator instances. Start a worker to begin.")
        } else {
            div()
                .flex_1()
                .flex_col()
                .p_2()
                .gap_1()
                .children(self.state.sessions.iter().map(instance_row))
        };

        let screens = div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .border_l_1()
            .border_color(rgb(0x3a3a3a))
            .child(
                div()
                    .w(px(400.0))
                    .h(px(240.0))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x3a3a3a))
                    .child("top screen"),
            )
            .child(
                div()
                    .w(px(320.0))
                    .h(px(240.0))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x3a3a3a))
                    .child("bottom screen"),
            );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe6e6e6))
            .child(header)
            .child(div().flex().flex_1().w_full().child(instances).child(screens))
    }
}

/// One row of the instance list.
fn instance_row(session: &SessionSummary) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .px_2()
        .py_1()
        .rounded_sm()
        .child(session.id.clone())
        .child(format!("{} / {}", session.model, session.renderer))
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1024.0), px(768.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| LotusWindow {
                state: LotusUiState::new(),
            }),
        )
        .expect("cannot open the Lotus window");

        cx.activate(true);
    });
}
