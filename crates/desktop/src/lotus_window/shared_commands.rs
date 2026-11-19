//! #129/#133/#138, handbook 12: one command model for the staged toolbar/menu.
//! Session IDs include creation/epoch; an old menu cannot retarget its actions.
use super::*;
use gpui::Entity;
use silvestris::command::CommandRegistry;
use silvestris::controls::{ConfirmationFocus, ConfirmationSheet};
use silvestris::core::command::{CommandDescriptor, CommandId, MenuEntry, MenuModel};
use silvestris::menu::PopupMenu;
use silvestris::ActiveTheme;

/// Key context of the main window's command scope. Text fields keep their
/// own editing keys (silvestris commands.md).
pub(super) const COMMAND_CONTEXT: &str = "LotusCommands";
const BINDING_PREDICATE: &str = "LotusCommands && !SilvestrisTextField";

pub(super) struct SharedCommands {
    pub registry: Entity<CommandRegistry>,
    pub menu: Entity<PopupMenu>,
    target: Option<SessionTarget>,
    session_ids: Vec<CommandId>,
    /// Commands whose handlers capture the model at the time a popup opens.
    popup_ids: Vec<CommandId>,
    /// Where the Actions menu last opened; Load reopens the menu there.
    anchor: gpui::Point<gpui::Pixels>,
    bound: bool,
    native_menu_state: Vec<CommandDescriptor>,
    confirmation: Option<PendingConfirmation>,
}

struct PendingConfirmation {
    command: SessionCommand,
    target: SessionTarget,
    focus: ConfirmationFocus,
    previous_focus: Option<gpui::FocusHandle>,
}

#[derive(Clone, Copy)]
pub(super) enum GlobalCommand {
    Sidebar,
    Inspector,
    Settings,
    Open,
    OpenTrace,
    Minimize,
    ZoomWindow,
    Keyboard,
    ZoomNext,
    ScreensNext,
    Zoom(Zoom),
    Screens(ScreenLayout),
}
impl GlobalCommand {
    const ALL: [Self; 15] = [
        Self::Sidebar,
        Self::Inspector,
        Self::Settings,
        Self::Open,
        Self::OpenTrace,
        Self::Minimize,
        Self::ZoomWindow,
        Self::Keyboard,
        Self::ZoomNext,
        Self::ScreensNext,
        Self::Zoom(Zoom::Fit),
        Self::Zoom(Zoom::Native),
        Self::Zoom(Zoom::Double),
        Self::Screens(ScreenLayout::Stacked),
        Self::Screens(ScreenLayout::SideBySide),
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Sidebar => "view.sidebar",
            Self::Inspector => "view.inspector",
            Self::Settings => "app.settings",
            Self::Open => "file.open",
            Self::OpenTrace => "debug.trace.open",
            Self::Minimize => "window.minimize",
            Self::ZoomWindow => "window.zoom",
            Self::Keyboard => "help.keyboard",
            Self::ZoomNext => "view.zoom.next",
            Self::ScreensNext => "view.screens.next",
            Self::Zoom(Zoom::Fit) => "view.zoom.fit",
            Self::Zoom(Zoom::Native) => "view.zoom.native",
            Self::Zoom(Zoom::Double) => "view.zoom.double",
            Self::Screens(ScreenLayout::Stacked) => "view.screens.stacked",
            Self::Screens(ScreenLayout::SideBySide) => "view.screens.side-by-side",
        }
    }
    fn descriptor(self, view: &LotusWindow) -> CommandDescriptor {
        match self {
            Self::Sidebar => CommandDescriptor::toggle(
                self.id(),
                "Sidebar",
                view.preferences.values.sessions_open,
            )
            .shortcut("secondary-b"),
            Self::Inspector => CommandDescriptor::toggle(
                self.id(),
                "Inspector",
                view.preferences.values.inspector_open,
            )
            .shortcut("secondary-i"),
            Self::Settings => CommandDescriptor::action(self.id(), "Settings")
                .needs_input()
                .shortcut("secondary-,"),
            Self::Open => CommandDescriptor::action(self.id(), "Open")
                .needs_input()
                .shortcut("secondary-o")
                .enabled(view.controller.is_some() && view.control),
            Self::OpenTrace => CommandDescriptor::action(self.id(), "Open JSONL Trace")
                .needs_input()
                .enabled(view.controller.is_some()),
            Self::Minimize => CommandDescriptor::action(self.id(), "Minimize Window"),
            Self::ZoomWindow => CommandDescriptor::action(self.id(), "Zoom Window"),
            Self::Keyboard => CommandDescriptor::action(self.id(), "Keyboard Shortcuts"),
            Self::ZoomNext => CommandDescriptor::action(self.id(), view.zoom.label()),
            Self::ScreensNext => CommandDescriptor::action(self.id(), view.layout.label()),
            Self::Zoom(zoom) => {
                CommandDescriptor::radio(self.id(), zoom.label(), "zoom", view.zoom == zoom)
            }
            Self::Screens(layout) => CommandDescriptor::radio(
                self.id(),
                layout.label(),
                "screens",
                view.layout == layout,
            ),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum SessionCommand {
    Run,
    Step,
    Load,
    Reset,
    Stop,
}
impl SessionCommand {
    const ALL: [Self; 5] = [Self::Run, Self::Step, Self::Load, Self::Reset, Self::Stop];
    fn key(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Step => "step",
            Self::Load => "load",
            Self::Reset => "reset",
            Self::Stop => "stop",
        }
    }
    pub fn id(self, target: Option<&SessionTarget>) -> CommandId {
        CommandId::new(match target {
            Some(t) => format!(
                "session.{}.{}:{}:{}:{}",
                self.key(),
                t.id.len(),
                t.id,
                t.created_unix_ms,
                t.epoch
            ),
            None => format!("session.{}.none", self.key()),
        })
    }
    fn descriptor(self, view: &LotusWindow, target: Option<&SessionTarget>) -> CommandDescriptor {
        let summary = target
            .filter(|t| view.selected == t.id)
            .and_then(|t| t.resolve(&view.sessions));
        let active = summary.is_some_and(|s| s.telemetry.state.is_active());
        let loaded = summary.is_some_and(|s| s.info.asset.is_some());
        let playing = summary.is_some_and(|s| s.telemetry.playing);
        let label = match self {
            Self::Run if playing => "Pause",
            Self::Run => "Run",
            Self::Step => "Step One Frame",
            Self::Load => "Load Content",
            Self::Reset => "Reset",
            Self::Stop => "Stop",
        };
        let enabled = view.control
            && summary.is_some()
            && match self {
                Self::Run | Self::Reset => active && loaded,
                Self::Step => active && loaded && !playing,
                Self::Load => active,
                Self::Stop => true,
            };
        let mut d = CommandDescriptor::action(self.id(target).as_str(), label).enabled(enabled);
        match self {
            Self::Run => d = d.shortcut("space"),
            Self::Step => d = d.shortcut("."),
            Self::Load => d = d.needs_input(),
            Self::Reset | Self::Stop => d = d.destructive(),
        }
        d
    }
}

impl SharedCommands {
    pub fn new(window: &mut Window, cx: &mut Context<LotusWindow>) -> Self {
        let registry = cx.new(|_| CommandRegistry::default());
        for command in GlobalCommand::ALL {
            let owner = cx.weak_entity();
            registry.update(cx, |registry, cx| {
                registry
                    .register(
                        CommandDescriptor::action(command.id(), command.id()).enabled(false),
                        move |_, window, cx| {
                            let _ = owner
                                .update(cx, |view, cx| view.invoke_global(command, window, cx));
                        },
                        cx,
                    )
                    .expect("unique global UI command")
            });
        }
        let menu = cx.new(|cx| {
            PopupMenu::new(
                MenuModel {
                    label: "Actions".into(),
                    entries: Vec::new(),
                },
                registry.clone(),
                window,
                cx,
            )
        });
        // Menu state invalidation must repaint its mounted deferred overlay.
        cx.observe(&menu, |_, _, cx| cx.notify()).detach();
        Self {
            registry,
            menu,
            target: None,
            session_ids: Vec::new(),
            popup_ids: Vec::new(),
            anchor: gpui::Point::default(),
            bound: false,
            native_menu_state: Vec::new(),
            confirmation: None,
        }
    }
}

impl LotusWindow {
    pub(super) fn sync_shared_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.selected_summary().map(SessionTarget::from_summary);
        let registry = self.shared_commands.registry.clone();
        if self.shared_commands.target != target || self.shared_commands.session_ids.is_empty() {
            // menus.md: the consumer invalidates an open menu whose target was
            // replaced; its rows would otherwise name removed command IDs.
            let menu = self.shared_commands.menu.clone();
            if menu.read(cx).is_open() {
                menu.update(cx, |menu, cx| menu.dismiss(window, cx, true));
            }
            for id in self
                .shared_commands
                .session_ids
                .drain(..)
                .chain(self.shared_commands.popup_ids.drain(..))
            {
                registry.update(cx, |r, cx| r.remove(&id, cx));
            }
            for command in SessionCommand::ALL {
                let snapshot = target.clone();
                let owner = cx.weak_entity();
                let descriptor = command.descriptor(self, snapshot.as_ref());
                self.shared_commands.session_ids.push(descriptor.id.clone());
                registry.update(cx, |r, cx| {
                    r.register(
                        descriptor,
                        move |_, window, cx| {
                            let _ = owner.update(cx, |view, cx| {
                                view.invoke_session(command, snapshot.as_ref(), window, cx)
                            });
                        },
                        cx,
                    )
                    .expect("unique scoped session command")
                });
            }
            self.shared_commands.target = target.clone();
        }
        for command in GlobalCommand::ALL {
            let descriptor = command.descriptor(self);
            registry.update(cx, |r, cx| {
                r.publish(descriptor, cx)
                    .expect("registered global UI command")
            });
        }
        for command in SessionCommand::ALL {
            let descriptor = command.descriptor(self, target.as_ref());
            registry.update(cx, |r, cx| {
                r.publish(descriptor, cx)
                    .expect("registered session UI command")
            });
        }
        if !self.shared_commands.bound {
            self.shared_commands.bound = true;
            // Global shortcuts only: session IDs change with every target, and
            // Space/period (reserved keys) stay with the root key handler, which
            // acts only while the window itself is focused so a focused control
            // keeps Space (key_down; lotus_input::layout::RESERVED_KEYS).
            let bindings = registry.read(cx).bindings_where(
                BINDING_PREDICATE,
                |d| !d.id.as_str().starts_with("session."),
                cx,
            );
            match bindings {
                Ok(bindings) => cx.bind_keys(bindings),
                Err(error) => tracing::warn!("command shortcuts not installed: {error}"),
            }
        }
        if cfg!(target_os = "macos") {
            let descriptors = registry.read(cx).descriptors();
            let state: Vec<_> = descriptors
                .iter()
                .filter(|descriptor| !self.shared_commands.popup_ids.contains(&descriptor.id))
                .cloned()
                .collect();
            if self.shared_commands.native_menu_state != state {
                self.shared_commands.native_menu_state = state;
                let mut menus = silvestris::menu::native_menus(&self.command_menus(), descriptors);
                menus.insert(2, silvestris::menu::native_edit_menu());
                cx.set_menus(menus);
            }
        }
    }

    fn invoke_global(
        &mut self,
        command: GlobalCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !command.descriptor(self).enabled {
            return;
        }
        self.release_input();
        self.menu = Menu::Closed;
        match command {
            GlobalCommand::Sidebar => self.toggle_pane(Pane::Sessions, window),
            GlobalCommand::Inspector => self.toggle_pane(Pane::Inspector, window),
            GlobalCommand::Settings => {
                self.open_settings(crate::settings_window::Target::Global, cx)
            }
            GlobalCommand::Open => {
                let session = self
                    .selected_summary()
                    .filter(|s| s.telemetry.state.is_active())
                    .map(|s| s.info.id.clone());
                self.open_file(session, cx);
            }
            GlobalCommand::OpenTrace => self.open_trace(cx),
            GlobalCommand::Minimize => window.minimize_window(),
            GlobalCommand::ZoomWindow => silvestris::native::zoom_window(window),
            GlobalCommand::Keyboard => {
                self.clear_popup_commands(cx);
                let model = MenuModel {
                    label: "Keyboard Shortcuts".into(),
                    entries: KEYS
                        .iter()
                        .map(|(key, action)| MenuEntry::Heading(format!("{key}  {action}")))
                        .collect(),
                };
                let y = cx.theme().metrics.titlebar_height + cx.theme().metrics.toolbar_height;
                self.open_popup_at(model, gpui::point(px(10.0), px(y)), window, cx);
            }
            GlobalCommand::ZoomNext => self.zoom = self.zoom.next(),
            GlobalCommand::ScreensNext => self.layout = self.layout.other(),
            GlobalCommand::Zoom(zoom) => self.zoom = zoom,
            GlobalCommand::Screens(layout) => self.layout = layout,
        }
        cx.notify();
    }

    fn invoke_session(
        &mut self,
        command: SessionCommand,
        target: Option<&SessionTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !command.descriptor(self, target).enabled {
            return;
        }
        let Some(target) = target else {
            return;
        };
        let Some(summary) = target.resolve(&self.sessions) else {
            return;
        };
        let id = summary.info.id.clone();
        let playing = summary.telemetry.playing;
        self.release_input();
        self.menu = Menu::Closed;
        match command {
            SessionCommand::Run => self.send(if playing {
                Command::Pause(id)
            } else {
                Command::Play(id)
            }),
            SessionCommand::Step => self.send(Command::Step(id)),
            SessionCommand::Load => self.open_load_menu(target.clone(), window, cx),
            SessionCommand::Reset | SessionCommand::Stop => {
                self.confirm_session_action(command, target.clone(), window, cx)
            }
        }
        cx.notify();
    }

    pub(super) fn confirm_session_action(
        &mut self,
        command: SessionCommand,
        target: SessionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.control
            || target.resolve(&self.sessions).is_none()
            || self.shared_commands.confirmation.is_some()
        {
            return;
        }
        self.release_input();
        let focus = ConfirmationFocus::new(cx);
        let previous_focus = window.focused(cx);
        focus.focus_cancel(window, cx);
        self.shared_commands.confirmation = Some(PendingConfirmation {
            command,
            target,
            focus,
            previous_focus,
        });
        self.shared_sources.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(super) fn has_confirmation(&self) -> bool {
        self.shared_commands.confirmation.is_some()
    }

    pub(super) fn render_confirmation(&self, cx: &Context<Self>) -> Option<ConfirmationSheet> {
        let pending = self.shared_commands.confirmation.as_ref()?;
        let summary = pending.target.resolve(&self.sessions);
        let name = summary
            .map(|summary| session_tree::session_label(&summary.info))
            .unwrap_or_else(|| pending.target.id.clone());
        let (title, message, action) = match pending.command {
            SessionCommand::Reset => (
                format!("Reset {name}?"),
                "The current guest restarts.",
                "Reset",
            ),
            SessionCommand::Stop
                if summary.is_some_and(|summary| {
                    matches!(summary.info.kind, SessionKind::Primary { .. })
                }) =>
            {
                (
                    format!("Stop {name}?"),
                    "The session closes and its card is kept.",
                    "Stop",
                )
            }
            SessionCommand::Stop => (
                format!("Stop {name}?"),
                "The session closes and its temporary card is discarded.",
                "Stop",
            ),
            _ => return None,
        };
        let owner = cx.weak_entity();
        Some(ConfirmationSheet::new(
            title,
            message,
            action,
            pending.focus.clone(),
            move |accepted, window, cx| {
                let _ = owner.update(cx, |view, cx| {
                    let Some(pending) = view.shared_commands.confirmation.take() else {
                        return;
                    };
                    view.shared_sources.update(cx, |_, cx| cx.notify());
                    if let Some(focus) = pending.previous_focus {
                        window.focus(&focus, cx);
                    } else {
                        window.blur(cx);
                    }
                    if accepted && view.control && pending.target.resolve(&view.sessions).is_some()
                    {
                        match pending.command {
                            SessionCommand::Stop => view.send(Command::Stop(pending.target.id)),
                            SessionCommand::Reset => view.send(Command::Reset(pending.target.id)),
                            _ => {}
                        }
                    }
                    cx.notify();
                });
            },
        ))
    }

    pub(super) fn session_command_id(&self, command: SessionCommand) -> CommandId {
        command.id(self
            .selected_summary()
            .map(SessionTarget::from_summary)
            .as_ref())
    }

    pub(super) fn clear_popup_commands(&mut self, cx: &mut Context<Self>) {
        let registry = self.shared_commands.registry.clone();
        for id in self.shared_commands.popup_ids.drain(..) {
            registry.update(cx, |r, cx| r.remove(&id, cx));
        }
    }

    pub(super) fn popup_action(
        &mut self,
        descriptor: CommandDescriptor,
        invoke: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> MenuEntry {
        let id = descriptor.id.clone();
        let owner = cx.weak_entity();
        self.shared_commands.registry.update(cx, |registry, cx| {
            registry
                .register(
                    descriptor,
                    move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            invoke(view, window, cx);
                            cx.notify();
                        });
                    },
                    cx,
                )
                .expect("unique popup command");
        });
        self.shared_commands.popup_ids.push(id.clone());
        MenuEntry::Command(id)
    }

    pub(super) fn open_popup_at(
        &mut self,
        model: MenuModel,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.release_input();
        self.menu = Menu::Closed;
        self.shared_commands.menu.update(cx, |menu, cx| {
            menu.set_model(model, window, cx);
            menu.open(position, window, cx);
        });
    }

    pub(super) fn open_new_session_menu(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_popup_commands(cx);
        let cards = self.cards.clone();
        let mut entries = vec![MenuEntry::Heading("Persistent Card".into())];
        for (index, card) in cards.iter().enumerate() {
            if self.card_is_mounted(card) {
                continue;
            }
            let name = card.clone();
            entries.push(self.popup_action(
                CommandDescriptor::action(&format!("new.open.{index}"), card),
                move |view, _, _| {
                    if view.control && view.cards.contains(&name) && !view.card_is_mounted(&name) {
                        view.create(SessionKind::Primary { card: name.clone() }, None);
                    }
                },
                cx,
            ));
        }
        let fresh = session_tree::next_card_name(&cards);
        entries.push(self.popup_action(
            CommandDescriptor::action("new.card", &format!("New Card ({fresh})")),
            move |view, _, _| {
                if view.control && !view.cards.contains(&fresh) && !view.card_is_mounted(&fresh) {
                    view.create(
                        SessionKind::Primary {
                            card: fresh.clone(),
                        },
                        None,
                    );
                }
            },
            cx,
        ));
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::Heading("Temporary Session".into()));
        entries.push(self.popup_action(
            CommandDescriptor::action("new.empty", "Blank Card"),
            |view, _, _| {
                if view.control {
                    view.create(SessionKind::Secondary { parent: None }, None);
                }
            },
            cx,
        ));
        for (index, card) in cards.into_iter().enumerate() {
            entries.push(self.popup_action(
                CommandDescriptor::action(&format!("new.copy.{index}"), &format!("Copy of {card}")),
                move |view, _, _| {
                    if view.control && view.cards.contains(&card) {
                        view.create(
                            SessionKind::Secondary {
                                parent: Some(card.clone()),
                            },
                            None,
                        );
                    }
                },
                cx,
            ));
        }
        self.open_popup_at(
            MenuModel {
                label: "New Session".into(),
                entries,
            },
            event.position(),
            window,
            cx,
        );
    }

    fn card_is_mounted(&self, name: &str) -> bool {
        self.sessions.iter().any(
            |summary| matches!(&summary.info.kind, SessionKind::Primary { card } if card == name),
        )
    }

    pub(super) fn open_inspector_utilities(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_popup_commands(cx);
        let target = self.inspection_target();
        let can_debug = self.control && target.is_some();
        let mut entries = vec![
            MenuEntry::Command(GlobalCommand::OpenTrace.id().into()),
            self.popup_action(
                CommandDescriptor::action("inspector.clipboard", "Run Clipboard JSON")
                    .enabled(can_debug),
                move |view, _, cx| view.run_clipboard_command(&target, cx),
                cx,
            ),
            MenuEntry::Separator,
            MenuEntry::Heading("Keyboard".into()),
        ];
        entries.extend(
            KEYS.iter()
                .map(|(key, action)| MenuEntry::Heading(format!("{key}  {action}"))),
        );
        self.open_popup_at(
            MenuModel {
                label: "Inspector Utilities".into(),
                entries,
            },
            event.position(),
            window,
            cx,
        );
    }

    fn command_menus(&self) -> Vec<MenuModel> {
        let session = |command| MenuEntry::Command(self.session_command_id(command));
        let global = |command: GlobalCommand| MenuEntry::Command(command.id().into());
        vec![
            MenuModel {
                label: "Lotus".into(),
                entries: vec![global(GlobalCommand::Settings)],
            },
            MenuModel {
                label: "File".into(),
                entries: vec![global(GlobalCommand::Open), session(SessionCommand::Load)],
            },
            MenuModel {
                label: "View".into(),
                entries: vec![
                    global(GlobalCommand::Sidebar),
                    global(GlobalCommand::Inspector),
                    MenuEntry::Submenu {
                        label: "Zoom".into(),
                        entries: [Zoom::Fit, Zoom::Native, Zoom::Double]
                            .map(|value| global(GlobalCommand::Zoom(value)))
                            .into(),
                    },
                    MenuEntry::Submenu {
                        label: "Screens".into(),
                        entries: [ScreenLayout::Stacked, ScreenLayout::SideBySide]
                            .map(|value| global(GlobalCommand::Screens(value)))
                            .into(),
                    },
                ],
            },
            MenuModel {
                label: "Session".into(),
                entries: vec![
                    session(SessionCommand::Run),
                    session(SessionCommand::Step),
                    MenuEntry::Separator,
                    session(SessionCommand::Reset),
                    session(SessionCommand::Stop),
                ],
            },
            MenuModel {
                label: "Debug".into(),
                entries: vec![global(GlobalCommand::OpenTrace)],
            },
            MenuModel {
                label: "Window".into(),
                entries: vec![
                    global(GlobalCommand::Minimize),
                    global(GlobalCommand::ZoomWindow),
                ],
            },
            MenuModel {
                label: "Help".into(),
                entries: vec![global(GlobalCommand::Keyboard)],
            },
        ]
    }

    pub(super) fn open_shared_actions(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.release_input();
        self.menu = Menu::Closed;
        self.sync_shared_commands(window, cx);
        self.clear_popup_commands(cx);
        let entries = self
            .command_menus()
            .into_iter()
            .map(|menu| MenuEntry::Submenu {
                label: menu.label,
                entries: menu.entries,
            })
            .collect();
        let model = MenuModel {
            label: "Actions".into(),
            entries,
        };
        let metrics = &cx.theme().metrics;
        let position = gpui::point(
            event.position().x,
            px(metrics.titlebar_height + metrics.toolbar_height),
        );
        self.shared_commands.anchor = position;
        self.shared_commands.menu.update(cx, |menu, cx| {
            menu.set_model(model, window, cx);
            menu.open(position, window, cx);
        });
    }
}

impl LotusWindow {
    /// Load Content: the host's approved assets, then Open… (replaces the
    /// legacy load popover). Each asset is a command scoped to `target`, so a
    /// menu left open across a reset cannot load into the new guest.
    pub(super) fn open_inline_load(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shared_commands.anchor = event.position();
        let target = self.selected_summary().map(SessionTarget::from_summary);
        self.invoke_session(SessionCommand::Load, target.as_ref(), window, cx);
    }

    fn open_load_menu(
        &mut self,
        target: SessionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_popup_commands(cx);
        let registry = self.shared_commands.registry.clone();
        let mut entries = vec![MenuEntry::Heading("Approved Content".into())];
        let scope = format!(
            "{}:{}:{}:{}",
            target.id.len(),
            target.id,
            target.created_unix_ms,
            target.epoch
        );
        if self.assets.is_empty() {
            let id = CommandId::new(format!("session.load-none.{scope}"));
            registry.update(cx, |r, cx| {
                r.register(
                    CommandDescriptor::action(id.as_str(), "None Configured for This Host")
                        .enabled(false),
                    |_, _, _| {},
                    cx,
                )
                .expect("unique asset placeholder")
            });
            self.shared_commands.popup_ids.push(id.clone());
            entries.push(MenuEntry::Command(id));
        }
        for (index, asset) in self.assets.iter().enumerate() {
            let id = CommandId::new(format!("session.load-asset.{index}.{scope}"));
            let label = session_tree::asset_label(asset);
            let (owner, target, asset) = (cx.weak_entity(), target.clone(), asset.clone());
            registry.update(cx, |r, cx| {
                r.register(
                    CommandDescriptor::action(id.as_str(), &label)
                        .tooltip(&asset)
                        .enabled(self.control),
                    move |_, _, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            // Revalidate: the session must still be this target.
                            if view.control && target.resolve(&view.sessions).is_some() {
                                view.send(Command::Load {
                                    session: target.id.clone(),
                                    asset: asset.clone(),
                                });
                            }
                            cx.notify();
                        });
                    },
                    cx,
                )
                .expect("unique asset command")
            });
            self.shared_commands.popup_ids.push(id.clone());
            entries.push(MenuEntry::Command(id));
        }
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::Command(GlobalCommand::Open.id().into()));
        let model = MenuModel {
            label: "Load Content".into(),
            entries,
        };
        let position = self.shared_commands.anchor;
        self.shared_commands.menu.update(cx, |menu, cx| {
            menu.set_model(model, window, cx);
            menu.open(position, window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};
    use silvestris::command::Invocation;

    fn session(id: &str, epoch: u64) -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.state = SessionState::Paused;
        telemetry.epoch = epoch;
        SessionSummary {
            info: SessionInfo {
                id: id.into(),
                kind: SessionKind::Secondary { parent: None },
                asset: Some("fixture.3dsx".into()),
                created_unix_ms: 1,
            },
            telemetry,
        }
    }
    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[test]
    fn actions_menu_shares_toolbar_commands_and_drops_a_replaced_target() {
        for theme in silvestris::ThemeId::ALL {
            let mut app = TestAppContext::single();
            app.update(|cx| {
                silvestris::test_support::initialize(
                    silvestris::ThemePreference::explicit(theme),
                    cx,
                )
            });
            let (view, cx) = app.add_window_view(|window, cx| {
                let mut view = LotusWindow::test_preview(window, cx);
                view.sessions = vec![session("a", 1)];
                view.selected = "a".into();
                view.control = true;
                view
            });
            cx.simulate_resize(gpui::size(px(1280.), px(880.)));
            draw(cx);
            let (registry, menu, stale) = cx.update(|_, cx| {
                let view = view.read(cx);
                (
                    view.shared_commands.registry.clone(),
                    view.shared_commands.menu.clone(),
                    view.session_command_id(SessionCommand::Run),
                )
            });
            // The toolbar Run button and the menu row read one descriptor.
            cx.update(|_, cx| {
                let run = registry
                    .read(cx)
                    .descriptors()
                    .get(&stale)
                    .cloned()
                    .unwrap();
                assert!(run.enabled && run.label == "Run", "{theme:?}");
            });
            let actions = cx.debug_bounds("shared-actions").unwrap();
            cx.simulate_click(actions.center(), Modifiers::default());
            draw(cx);
            cx.update(|_, cx| assert!(menu.read(cx).is_open(), "{theme:?}"));

            // A guest reset (new epoch) replaces the target the menu was opened for.
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    view.sessions = vec![session("a", 2)];
                    cx.notify();
                })
            });
            draw(cx);
            cx.update(|window, cx| {
                assert!(!menu.read(cx).is_open(), "{theme:?}");
                assert_eq!(
                    CommandRegistry::invoke(&registry, &stale, window, cx),
                    Invocation::Missing
                );
                let fresh = view.read(cx).session_command_id(SessionCommand::Run);
                assert_ne!(fresh, stale);
                assert!(registry.read(cx).descriptors().get(&fresh).unwrap().enabled);
            });
        }
    }

    #[test]
    fn destructive_commands_use_a_keyboard_modal_sheet() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::test_preview(window, cx);
            view.sessions = vec![session("a", 1)];
            view.selected = "a".into();
            view.control = true;
            view
        });
        draw(cx);
        let before = cx.update(|window, cx| {
            let before = window.focused(cx);
            let state = view.read(cx);
            let reset = state.session_command_id(SessionCommand::Reset);
            let registry = state.shared_commands.registry.clone();
            assert_eq!(
                CommandRegistry::invoke(&registry, &reset, window, cx),
                Invocation::Dispatched
            );
            before
        });
        draw(cx);
        assert!(cx.debug_bounds("sheet-cancel").is_some());
        cx.simulate_keystrokes("tab z ctrl-i");
        draw(cx);
        cx.update(|_, cx| {
            let state = view.read(cx);
            assert!(state.shared_commands.confirmation.is_some());
            assert!(state.keys.is_empty());
            assert!(!state.preferences.values.inspector_open);
        });
        cx.simulate_keystrokes("escape");
        draw(cx);
        cx.update(|window, cx| {
            assert!(view.read(cx).shared_commands.confirmation.is_none());
            assert_eq!(window.focused(cx), before);
        });
    }

    #[test]
    fn load_opens_the_shared_menu_with_target_scoped_assets() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::AQUA_LIGHT),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::test_preview(window, cx);
            view.sessions = vec![session("a", 1)];
            view.selected = "a".into();
            view.control = true;
            view.assets = vec!["one.3ds".into(), "two.3ds".into()];
            view
        });
        cx.simulate_resize(gpui::size(px(1280.), px(880.)));
        draw(cx);
        let (registry, menu, load) = cx.update(|_, cx| {
            let view = view.read(cx);
            (
                view.shared_commands.registry.clone(),
                view.shared_commands.menu.clone(),
                view.session_command_id(SessionCommand::Load),
            )
        });
        cx.update(|window, cx| {
            assert_eq!(
                CommandRegistry::invoke(&registry, &load, window, cx),
                Invocation::Dispatched
            );
        });
        draw(cx);
        let assets = cx.update(|_, cx| {
            assert!(menu.read(cx).is_open());
            assert_eq!(view.read(cx).menu, Menu::Closed, "no legacy popover");
            view.read(cx).shared_commands.popup_ids.clone()
        });
        assert_eq!(assets.len(), 2);
        cx.update(|_, cx| {
            let d = registry
                .read(cx)
                .descriptors()
                .get(&assets[0])
                .cloned()
                .unwrap();
            assert!(d.enabled);
        });
        // A reset replaces the target; its asset commands disappear with it.
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.sessions = vec![session("a", 2)];
                cx.notify();
            })
        });
        draw(cx);
        cx.update(|window, cx| {
            assert!(!menu.read(cx).is_open());
            assert_eq!(
                CommandRegistry::invoke(&registry, &assets[0], window, cx),
                Invocation::Missing
            );
        });
    }
}
