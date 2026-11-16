//! T02: four simulated sessions, native BGRA images, bounded replacement.
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Context, IntoElement, ParentElement, Render, RenderImage, Styled,
    Window, WindowBounds, WindowOptions,
};
use image::{Frame, RgbaImage};
use lotus_desktop::screen_geometry;

struct ScreenSpike {
    images: Vec<Arc<RenderImage>>,
    sequence: u64,
    started: Instant,
    host_generation_us: u128,
}

impl ScreenSpike {
    fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            if this
                .update(cx, |view, cx| {
                    view.update_images();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        let mut view = Self {
            images: Vec::new(),
            sequence: 0,
            started: Instant::now(),
            host_generation_us: 0,
        };
        view.update_images();
        view
    }

    fn update_images(&mut self) {
        let start = Instant::now();
        let mut images = Vec::with_capacity(8);
        for session in 0..4 {
            for width in [400, 320] {
                let pixels = screen_geometry::pattern(width, session, self.sequence as u8);
                let buffer = RgbaImage::from_raw(width, 240, pixels)
                    .expect("generated native image has exact dimensions");
                images.push(Arc::new(RenderImage::new(vec![Frame::new(buffer)])));
            }
        }
        self.images = images;
        self.sequence += 1;
        self.host_generation_us = start.elapsed().as_micros();
    }
}

impl Render for ScreenSpike {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Exercise selection and two display scales without retaining historical textures.
        let selected = (self.started.elapsed().as_secs() / 5 % 4) as usize;
        let scale = if (self.started.elapsed().as_secs() / 20).is_multiple_of(2) {
            1.0
        } else {
            1.5
        };
        let screens = gpui::div()
            .flex()
            .gap_2()
            .child(
                gpui::img(self.images[selected * 2].clone())
                    .w(gpui::px(400.0 * scale))
                    .h(gpui::px(240.0 * scale)),
            )
            .child(
                gpui::img(self.images[selected * 2 + 1].clone())
                    .w(gpui::px(320.0 * scale))
                    .h(gpui::px(240.0 * scale)),
            );
        gpui::div().flex().flex_col().size_full().p_4().gap_4()
            .bg(gpui::rgb(0x202028)).text_color(gpui::rgb(0xffffff))
            .child("Lotus T02 — synthetic BGRA display; no emulator")
            .child(format!("Session {}/4 | sequence {} | zoom {scale} | window DPI {} | host generation {} us | elapsed {} s", selected + 1, self.sequence, window.scale_factor(), self.host_generation_us, self.started.elapsed().as_secs()))
            .child(screens)
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, gpui::size(gpui::px(1200.0), gpui::px(700.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(ScreenSpike::new),
        )
        .expect("open screen spike window");
        cx.activate(true);
    });
}
