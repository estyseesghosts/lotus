//! LOTUS native session browser. GPUI only consumes owned snapshots.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Render, RenderImage,
    SharedString, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions,
};
use image::{Frame, RgbaImage};
use lotus_desktop::desktop_controller::{Command, DesktopController};
use lotus_desktop::screen_geometry::TouchTransform;
use lotus_session::native_session::InputState;
use lotus_session::supervisor::HostConfig;

#[derive(Default)]
struct SessionView {
    state: String,
    frame: u64,
    playing: bool,
    error: String,
    images: Vec<Arc<RenderImage>>,
}
#[derive(Clone, Copy)]
enum Layout {
    Stacked,
    SideBySide,
}
struct LotusWindow {
    controller: Option<DesktopController>,
    assets: Vec<String>,
    sessions: BTreeMap<String, SessionView>,
    selected: String,
    last_actor_selected: String,
    focus: FocusHandle,
    input: InputState,
    bottom_rect: Arc<Mutex<Option<TouchTransform>>>,
    zoom: f32,
    layout: Layout,
    error: String,
}

impl LotusWindow {
    fn new(config: Option<HostConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.on_blur(&focus, window, |view, _, _| view.release_input())
            .detach();
        cx.observe_window_activation(window, |view, window, _| {
            if !window.is_window_active() {
                view.release_input();
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            if this
                .update(cx, |view, cx| {
                    view.poll();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        let assets = config
            .as_ref()
            .map(|config| config.assets.keys().cloned().collect())
            .unwrap_or_default();
        let (controller, error) = match config.map(DesktopController::start).transpose() {
            Ok(controller) => (controller, String::new()),
            Err(error) => (None, error.to_string()),
        };
        Self {
            controller,
            assets,
            sessions: BTreeMap::new(),
            selected: String::new(),
            last_actor_selected: String::new(),
            focus,
            input: InputState::default(),
            bottom_rect: Arc::default(),
            zoom: 1.0,
            layout: Layout::Stacked,
            error,
        }
    }

    fn send(&mut self, command: Command) {
        if let Some(controller) = &self.controller {
            if let Err(error) = controller.commands.try_send(command) {
                self.error = format!("Control busy: {error}");
            }
        }
    }

    fn poll(&mut self) {
        let Some(controller) = &self.controller else {
            return;
        };
        let Ok(mut update) = controller.latest.lock() else {
            return;
        };
        self.error = update.error.clone();
        if update.selected != self.last_actor_selected {
            if let Ok(mut inputs) = controller.input.lock() {
                if !self.selected.is_empty() {
                    inputs.insert(self.selected.clone(), InputState::default());
                }
            }
            self.input = InputState::default();
            self.selected = update.selected.clone();
            self.last_actor_selected = update.selected.clone();
        }
        self.sessions
            .retain(|id, _| update.sessions.contains_key(id));
        for (id, snapshot) in &mut update.sessions {
            let view = self.sessions.entry(id.clone()).or_default();
            view.state = snapshot.state.clone();
            view.frame = snapshot.frame;
            view.playing = snapshot.playing;
            view.error = snapshot.error.clone();
            if !view.error.is_empty() {
                view.images.clear();
            }
            if let Some(frames) = snapshot.images.take() {
                view.images = frames
                    .into_iter()
                    .map(|frame| {
                        let image =
                            RgbaImage::from_raw(frame.lease.width, frame.lease.height, frame.bgra)
                                .expect("supervisor validated exact image dimensions");
                        Arc::new(RenderImage::new(vec![Frame::new(image)]))
                    })
                    .collect();
            }
        }
    }

    fn publish_input(&mut self) {
        if let Some(controller) = &self.controller {
            if let Ok(mut inputs) = controller.input.lock() {
                if self.sessions.contains_key(&self.selected) {
                    inputs.insert(self.selected.clone(), self.input);
                }
            }
        }
    }
    fn release_input(&mut self) {
        self.input = InputState::default();
        self.publish_input();
    }
    fn select(&mut self, id: String) {
        self.release_input();
        self.selected = id.clone();
        self.send(Command::Select(id));
    }
    fn key(&mut self, key: &str, down: bool) {
        let mask = match key {
            "z" => 1,
            "x" => 2,
            "a" => 4,
            "s" => 8,
            "up" => 16,
            "down" => 32,
            "left" => 64,
            "right" => 128,
            "q" => 256,
            "w" => 512,
            "enter" => 1024,
            "backspace" => 2048,
            _ => return,
        };
        if down {
            self.input.buttons |= mask;
        } else {
            self.input.buttons &= !mask;
        }
        self.publish_input();
    }
    fn touch(&mut self, x: f32, y: f32) {
        let coordinate = self
            .bottom_rect
            .lock()
            .ok()
            .and_then(|rect| rect.as_ref().and_then(|rect| rect.map_logical(x, y)));
        if let Some((x, y)) = coordinate {
            self.input.x = x;
            self.input.y = y;
            self.input.touch = true;
        } else {
            self.input.touch = false;
        }
        self.publish_input();
    }
}

fn button(
    label: &'static str,
    command: Command,
    cx: &mut Context<LotusWindow>,
) -> impl IntoElement {
    gpui::div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_sm()
        .bg(gpui::rgb(0x353947))
        .child(label)
        .on_click(cx.listener(move |view, _, _, _| view.send(command.clone())))
}

impl Render for LotusWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected.clone();
        let mut header = gpui::div().flex().gap_2().items_center().child("LOTUS");
        for asset in self.assets.clone() {
            let label = asset.clone();
            header = header.child(
                gpui::div()
                    .id(SharedString::from(format!("create-{asset}")))
                    .px_3()
                    .py_1()
                    .bg(gpui::rgb(0x353947))
                    .child(format!("New: {label}"))
                    .on_click(
                        cx.listener(move |view, _, _, _| view.send(Command::Create(asset.clone()))),
                    ),
            );
        }
        let playing = self
            .sessions
            .get(&selected)
            .is_some_and(|view| view.playing);
        header = header
            .child(button(
                if playing { "Pause" } else { "Run" },
                if playing {
                    Command::Pause(selected.clone())
                } else {
                    Command::Play(selected.clone())
                },
                cx,
            ))
            .child(button("Reset", Command::Reset(selected.clone()), cx))
            .child(button("Stop", Command::Stop(selected.clone()), cx))
            .child(
                gpui::div()
                    .id("zoom")
                    .px_3()
                    .child(format!("Zoom {}x", self.zoom))
                    .on_click(cx.listener(|view, _, _, _| {
                        view.release_input();
                        view.zoom = if view.zoom == 1.0 { 1.5 } else { 1.0 };
                    })),
            )
            .child(
                gpui::div()
                    .id("layout")
                    .px_3()
                    .child("Dock screens")
                    .on_click(cx.listener(|view, _, _, _| {
                        view.release_input();
                        view.layout = match view.layout {
                            Layout::Stacked => Layout::SideBySide,
                            Layout::SideBySide => Layout::Stacked,
                        };
                    })),
            );
        let mut browser = gpui::div().flex().flex_col().w(gpui::px(210.0)).gap_2();
        for (id, view) in &self.sessions {
            let id = id.clone();
            browser = browser.child(
                gpui::div()
                    .id(SharedString::from(id.clone()))
                    .p_2()
                    .bg(gpui::rgb(if id == selected { 0x45495c } else { 0x292d38 }))
                    .child(format!(
                        "{}… | {} | frame {}",
                        &id[..8],
                        view.state,
                        view.frame
                    ))
                    .on_click(cx.listener(move |view, _, _, _| view.select(id.clone()))),
            );
        }
        let images = self
            .sessions
            .get(&selected)
            .map(|view| view.images.clone())
            .unwrap_or_default();
        let mut top = gpui::div()
            .w(gpui::px(400.0 * self.zoom))
            .h(gpui::px(240.0 * self.zoom))
            .bg(gpui::rgb(0));
        if let Some(image) = images.first() {
            top = top.child(gpui::img(image.clone()).size_full());
        }
        let rect = self.bottom_rect.clone();
        let mut bottom = gpui::div()
            .id("touch-screen")
            .relative()
            .w(gpui::px(320.0 * self.zoom))
            .h(gpui::px(240.0 * self.zoom))
            .bg(gpui::rgb(0))
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        if let Ok(mut rect) = rect.lock() {
                            *rect = Some(TouchTransform {
                                left: bounds.origin.x.into(),
                                top: bounds.origin.y.into(),
                                width: bounds.size.width.into(),
                                height: bounds.size.height.into(),
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, _| {
                    view.touch(event.position.x.into(), event.position.y.into())
                }),
            )
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, _| {
                if view.input.touch && event.dragging() {
                    view.touch(event.position.x.into(), event.position.y.into());
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.input.touch = false;
                    view.publish_input();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.input.touch = false;
                    view.publish_input();
                }),
            );
        if let Some(image) = images.get(1) {
            bottom = bottom.child(gpui::img(image.clone()).size_full());
        }
        let mut screens = gpui::div().flex().gap_2().items_center();
        if matches!(self.layout, Layout::Stacked) {
            screens = screens.flex_col();
        }
        let error = self
            .sessions
            .get(&selected)
            .map(|view| view.error.clone())
            .filter(|error| !error.is_empty())
            .unwrap_or_else(|| self.error.clone());
        gpui::div().size_full().flex().flex_col().p_4().gap_3().bg(gpui::rgb(0x1d2028)).text_color(gpui::rgb(0xe6e7ef))
            .track_focus(&self.focus)
            .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, cx| window.focus(&view.focus, cx)))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, _| view.key(&event.keystroke.key, true)))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, _| view.key(&event.keystroke.key, false)))
            .child(header)
            .child(format!("AZAHAR / CITnLO • {}x display scale • Z/X = A/B • A/S = X/Y • arrows • Q/W = L/R • Enter/Backspace", window.scale_factor()))
            .child(if self.controller.is_none() { "Open LOTUS with a host configuration JSON to enable approved homebrew assets.".to_string() } else { error })
            .child(gpui::div().flex().gap_4().child(browser).child(screens.child(top).child(bottom)))
    }
}

fn main() {
    let config = std::env::args_os().nth(1).map(|path| {
        let text = std::fs::read_to_string(path).expect("read host configuration");
        serde_json::from_str::<HostConfig>(&text).expect("valid host configuration")
    });
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, gpui::size(gpui::px(1300.0), gpui::px(850.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("LOTUS".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| LotusWindow::new(config, window, cx)),
        )
        .expect("open LOTUS window");
        cx.activate(true);
    });
}
