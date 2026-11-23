use gpui::{div, prelude::*, px, size, App, Bounds, Context, Entity, Window, WindowHandle};
use lotus_mcp::tool_arguments::EvidenceTraceSlice;
use lotus_session::{
    investigation::{
        ArtifactState, Investigation, InvestigationDetails, InvestigationView, ReferenceState,
        SessionReference,
    },
    supervisor::Supervisor,
};
use silvestris::controls::{Button, TextField};
use std::{io, sync::Arc};

struct Opened(WindowHandle<InvestigationWindow>);
impl gpui::Global for Opened {}

pub fn open(cx: &mut App, host: Arc<Supervisor>, target: Option<SessionReference>) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        if handle
            .update(cx, |view, window, cx| {
                view.target = target.clone();
                window.activate_window();
                cx.notify();
            })
            .is_ok()
        {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(740.), px(540.)), cx);
    let options =
        silvestris::chrome::window_options("Investigations", bounds, size(px(560.), px(380.)), cx);
    match silvestris::chrome::open_window(options, cx, |window, cx| {
        cx.new(|cx| InvestigationWindow::new(host, target, window, cx))
    }) {
        Ok(handle) => cx.set_global(Opened(handle)),
        Err(error) => tracing::warn!(%error, "cannot open investigations"),
    }
}

pub fn close(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

struct InvestigationWindow {
    host: Arc<Supervisor>,
    target: Option<SessionReference>,
    records: Vec<Investigation>,
    selected: Option<InvestigationView>,
    draft: InvestigationDetails,
    title: Entity<TextField>,
    purpose: Entity<TextField>,
    next_steps: Entity<TextField>,
    busy: bool,
    status: String,
}

impl InvestigationWindow {
    fn new(
        host: Arc<Supervisor>,
        target: Option<SessionReference>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |label| {
            cx.new(|cx| {
                let mut field = TextField::new(window, cx);
                field.set_label(label, cx);
                field.set_placeholder(label, cx);
                field
            })
        };
        let mut view = Self {
            host,
            target,
            records: Vec::new(),
            selected: None,
            draft: InvestigationDetails::default(),
            title: field("Title"),
            purpose: field("Purpose"),
            next_steps: field("Next steps"),
            busy: false,
            status: String::new(),
        };
        view.refresh(cx);
        view
    }

    fn run<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(Arc<Supervisor>) -> io::Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status.clear();
        let host = self.host.clone();
        let task = cx.background_executor().spawn(async move { work(host) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(value) => apply(view, value, cx),
                    Err(error) => view.status = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.run(
            cx,
            |host| host.investigations(),
            |view, records, _| view.records = records,
        );
    }

    fn load(&mut self, id: String, cx: &mut Context<Self>) {
        self.run(
            cx,
            move |host| host.investigation(&id),
            |view, record, cx| view.edit(Some(record), cx),
        );
    }

    fn edit(&mut self, selected: Option<InvestigationView>, cx: &mut Context<Self>) {
        self.draft = selected
            .as_ref()
            .map(|view| view.record.details.clone())
            .unwrap_or_default();
        self.title
            .update(cx, |field, cx| field.set_text(&self.draft.title, cx));
        self.purpose
            .update(cx, |field, cx| field.set_text(&self.draft.purpose, cx));
        self.next_steps
            .update(cx, |field, cx| field.set_text(&self.draft.next_steps, cx));
        self.selected = selected;
        self.status.clear();
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let mut details = self.draft.clone();
        details.title = self.title.read(cx).text().into();
        details.purpose = self.purpose.read(cx).text().into();
        details.next_steps = self.next_steps.read(cx).text().into();
        let revision = self
            .selected
            .as_ref()
            .map(|view| (view.record.id.clone(), view.record.revision));
        self.run(
            cx,
            move |host| {
                let record = match revision {
                    Some((id, revision)) => host.update_investigation(&id, revision, details)?,
                    None => host.create_investigation(details)?,
                };
                Ok((host.investigations()?, host.investigation(&record.id)?))
            },
            |view, (records, record), _| {
                view.records = records;
                view.draft = record.record.details.clone();
                view.selected = Some(record);
                view.status = "Saved".into();
            },
        );
    }

    fn export(&mut self, trace: Option<EvidenceTraceSlice>, cx: &mut Context<Self>) {
        let Some(view) = &self.selected else { return };
        let id = view.record.id.clone();
        let revision = view.record.revision;
        self.run(
            cx,
            move |host| {
                lotus_mcp::evidence::export(&host, &id, revision, &[], trace.as_slice(), false)
            },
            |view, result, cx| {
                if let Some(uri) = result["uri"].as_str() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(uri.into()));
                    view.status = "Evidence archived. Bundle URI copied.".into();
                }
            },
        );
    }
}

impl Render for InvestigationWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::palette(cx);
        let ready = !self.busy;
        let editable = ready && self.host.allows_control();
        let selected = self.selected.as_ref();
        let mut list = div()
            .id("investigation-list")
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll();
        for record in &self.records {
            let id = record.id.clone();
            list = list.child(
                Button::new(id.clone(), record.details.title.clone())
                    .disabled(!ready)
                    .selected(selected.is_some_and(|view| view.record.id == id))
                    .on_click(cx.listener(move |view, _, _, cx| view.load(id.clone(), cx))),
            );
        }
        let mut details = div()
            .id("investigation-details")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w_0()
            .overflow_y_scroll();
        if let Some(view) = selected {
            details = details.child(format!("Revision {}", view.record.revision));
            for (trace, state) in view.record.details.traces.iter().zip(&view.trace_states) {
                let uri = trace.uri.clone();
                let available = matches!(state, ArtifactState::Available | ArtifactState::Pinned);
                details = details.child(
                    div().flex().flex_col().gap_1().child(uri.clone()).child(
                        Button::new(uri.clone(), "Archive with first 128 trace events")
                            .disabled(!editable || !available)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.export(
                                    Some(EvidenceTraceSlice {
                                        uri: uri.clone(),
                                        offset: 0,
                                        limit: 128,
                                    }),
                                    cx,
                                );
                            })),
                    ),
                );
            }
        }
        for (index, target) in self.draft.sessions.iter().enumerate() {
            let state = selected.and_then(|view| view.session_states.get(index));
            let label = match state {
                Some(ReferenceState::Live) => "Live",
                Some(ReferenceState::EpochChanged) => "Guest changed",
                Some(ReferenceState::Expired) => "Expired",
                None => "Pending save",
            };
            details = details.child(format!("{} · {label}", target.session));
        }
        let can_reference = self
            .target
            .as_ref()
            .is_some_and(|target| !self.draft.sessions.contains(target));
        let controls = div()
            .flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("investigation-save", "Save")
                    .disabled(!editable)
                    .on_click(cx.listener(|view, _, _, cx| view.save(cx))),
            )
            .child(
                Button::new("investigation-reference", "Add session")
                    .disabled(!editable || !can_reference)
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(target) = &view.target {
                            if !view.draft.sessions.contains(target) {
                                view.draft.sessions.push(target.clone());
                                cx.notify();
                            }
                        }
                    })),
            )
            .child(
                Button::new("investigation-export", "Archive saved revision")
                    .disabled(!editable || selected.is_none())
                    .on_click(cx.listener(|view, _, _, cx| view.export(None, cx))),
            );
        details = details
            .child(self.title.clone())
            .child(self.purpose.clone())
            .child(self.next_steps.clone())
            .child(controls)
            .child(self.status.clone());
        div()
            .size_full()
            .bg(theme.window)
            .text_color(theme.text)
            .flex()
            .gap_4()
            .p_4()
            .child(
                div()
                    .w(px(220.))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("investigation-new", "New")
                                    .disabled(!editable)
                                    .on_click(cx.listener(|view, _, _, cx| view.edit(None, cx))),
                            )
                            .child(
                                Button::new("investigation-refresh", "Refresh list")
                                    .disabled(!ready)
                                    .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                            ),
                    )
                    .child(list),
            )
            .child(details)
    }
}
