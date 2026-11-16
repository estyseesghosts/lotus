//! Discord Rich Presence ("Now Playing") for the desktop host. Opt-in.
//!
//! Policy ([`derive`]): count *live loaded* sessions, meaning Running or Paused
//! with content (a session paused at a breakpoint still counts; `playing` is
//! ignored). Zero clears the activity. One reads `Playing NAME...`. Two or more
//! running the same content read `Debugging NAME...`; different content reads
//! `Debugging multiple games`. Content is compared by its resolved canonical
//! path (the supervisor's approved asset), not the display name, so two
//! different files called `game.3dsx` are different games. Only the file
//! name is sent, never a path; with titles hidden, `Playing a game` /
//! `Debugging sessions`.
//!
//! Transport: `discord-rich-presence` 1.1.0 (MIT), Discord's local IPC pipe.
//! Its calls block, so they run on one dedicated thread. The supervisor loop
//! hands it the latest desired state through [`Presence::publish`], which
//! only wakes the thread when that state changes. Connection failures retry
//! with backoff (5 s doubling to 60 s), logging at warn level only once per
//! outage. Disabled means the thread never connects.
//!
//! Field mapping from Discord's Rich Presence guide
//! (discord-api-docs `developers/discord-social-sdk/development-guides/setting-rich-presence.mdx`):
//! the application name is the activity title, `details` is the first line,
//! `state` the second.
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use lotus_session::supervisor::SessionSummary;
use lotus_session::SessionState;

use crate::preferences::Preferences;

/// What a live loaded session contributes: canonical identity and display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub identity: String,
    pub title: String,
}

/// The activity to show; `None` clears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub details: String,
    pub state: Option<String>,
}

/// Content of every live loaded session. `resolve` maps an asset handle to
/// `(canonical identity, file name)`; unresolvable handles fall back to the
/// handle itself.
pub fn live_content(
    sessions: &[SessionSummary],
    mut resolve: impl FnMut(&str) -> Option<(String, String)>,
) -> Vec<Content> {
    sessions
        .iter()
        .filter(|s| {
            matches!(
                s.telemetry.state,
                SessionState::Running | SessionState::Paused
            )
        })
        .filter_map(|s| s.info.asset.as_deref())
        .map(|handle| {
            let (identity, title) = resolve(handle).unwrap_or_else(|| {
                (
                    handle.to_string(),
                    handle
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(handle)
                        .to_string(),
                )
            });
            Content {
                identity,
                title: sanitize(&title),
            }
        })
        .collect()
}

/// Discord limits `details` to 128 characters; keep the name well inside it
/// and drop control characters.
fn sanitize(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    if name.chars().count() > 96 {
        format!("{}…", name.chars().take(95).collect::<String>())
    } else {
        name
    }
}

#[must_use]
pub fn derive(content: &[Content], hide_titles: bool) -> Option<Activity> {
    let first = content.first()?;
    if content.len() == 1 {
        return Some(Activity {
            details: if hide_titles {
                "Playing a game".into()
            } else {
                format!("Playing {}...", first.title)
            },
            state: None,
        });
    }
    let same = content.iter().all(|c| c.identity == first.identity);
    Some(Activity {
        details: match (hide_titles, same) {
            (true, _) => "Debugging sessions".into(),
            (false, true) => format!("Debugging {}...", first.title),
            (false, false) => "Debugging multiple games".into(),
        },
        state: Some(format!("{} sessions", content.len())),
    })
}

/// The project's registered Discord application ("LOTUS"), owned by the
/// project owner. Builds can override it with `LOTUS_DISCORD_APPLICATION_ID`.
pub const DEFAULT_APPLICATION_ID: &str = "1572001204404363274";

/// Application ID: the preference if set, else the build override, else
/// [`DEFAULT_APPLICATION_ID`]. Discord IDs are decimal snowflakes.
#[must_use]
pub fn application_id(preferences: &Preferences) -> Option<String> {
    let id = preferences
        .discord_application_id
        .clone()
        .unwrap_or_else(|| {
            option_env!("LOTUS_DISCORD_APPLICATION_ID")
                .unwrap_or(DEFAULT_APPLICATION_ID)
                .to_string()
        });
    Some(id).filter(|id| !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()))
}

/// Desired state shared with the presence thread.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Desired {
    /// `Some` when enabled with a usable application ID.
    application: Option<String>,
    hide_titles: bool,
    content: Vec<Content>,
    shutdown: bool,
}

/// What the presence thread last achieved, for the preferences menu and tests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Off,
    /// Discord is not reachable; retrying.
    Waiting(String),
    /// Discord confirmed this activity (`None`: cleared).
    Shown(Option<Activity>),
}

struct Shared {
    desired: Mutex<Desired>,
    changed: Condvar,
    status: Mutex<Status>,
}

/// Handle owned by the desktop controller.
pub struct Presence {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// One open IPC connection. Separated from the thread's state machine so
/// tests can script Discord being absent, failing or restarting.
trait Link: Send {
    /// Set (`Some`) or clear (`None`) the activity and wait for Discord's reply.
    fn send(&mut self, activity: Option<&Activity>) -> Result<(), String>;
    fn close(&mut self);
}

trait Connector: Send {
    fn connect(&mut self, application: &str) -> Result<Box<dyn Link>, String>;
}

struct Discord;

impl Connector for Discord {
    fn connect(&mut self, application: &str) -> Result<Box<dyn Link>, String> {
        let mut client = DiscordIpcClient::new(application);
        client.connect().map_err(|e| e.to_string())?;
        Ok(Box::new(client))
    }
}

impl Link for DiscordIpcClient {
    /// The crate never reads after its handshake, so read the reply here to
    /// notice a dead pipe or an ERROR event.
    fn send(&mut self, activity: Option<&Activity>) -> Result<(), String> {
        let result = match activity {
            Some(activity) => {
                let mut payload = activity::Activity::new().details(activity.details.as_str());
                if let Some(state) = &activity.state {
                    payload = payload.state(state.as_str());
                }
                self.set_activity(payload)
            }
            None => self.clear_activity(),
        };
        result.map_err(|e| e.to_string())?;
        let (_, reply) = self.recv().map_err(|e| e.to_string())?;
        if reply["evt"] == "ERROR" {
            return Err(reply["data"]["message"]
                .as_str()
                .unwrap_or("Discord refused the activity")
                .to_string());
        }
        Ok(())
    }

    fn close(&mut self) {
        let _ = DiscordIpc::close(self);
    }
}

impl Presence {
    #[must_use]
    pub fn start() -> Self {
        Self::start_with(Box::new(Discord), RETRY_MIN)
    }

    fn start_with(connector: Box<dyn Connector>, retry_min: Duration) -> Self {
        let shared = Arc::new(Shared {
            desired: Mutex::new(Desired::default()),
            changed: Condvar::new(),
            status: Mutex::new(Status::Off),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("lotus-presence".into())
            .spawn(move || run(&worker, connector, retry_min))
            .ok();
        Self { shared, thread }
    }

    /// Enable/disable and title hiding, from the user's preferences.
    pub fn configure(&self, preferences: &Preferences) {
        let application = preferences
            .discord_presence
            .then(|| application_id(preferences))
            .flatten();
        self.update(|desired| {
            desired.application = application;
            desired.hide_titles = preferences.discord_hide_titles;
        });
    }

    /// Latest live content; wakes the thread only when it differs.
    pub fn publish(&self, content: Vec<Content>) {
        self.update(|desired| desired.content = content);
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.shared
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    fn update(&self, change: impl FnOnce(&mut Desired)) {
        let Ok(mut desired) = self.shared.desired.lock() else {
            return;
        };
        let before = desired.clone();
        change(&mut desired);
        if *desired != before {
            self.shared.changed.notify_one();
        }
    }
}

impl Drop for Presence {
    /// Clears the activity and closes the pipe. Bounded: the thread's only
    /// blocking calls are pipe reads that Discord answers immediately or
    /// that fail when it exits; if it does not finish within 2 s it is
    /// left to end with the process.
    fn drop(&mut self) {
        self.update(|desired| desired.shutdown = true);
        if let Some(thread) = self.thread.take() {
            let (done, wait) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = thread.join();
                let _ = done.send(());
            });
            let _ = wait.recv_timeout(Duration::from_secs(2));
        }
    }
}

struct Connection {
    link: Box<dyn Link>,
    application: String,
}

fn close(connection: Option<Connection>, clear: bool) {
    if let Some(mut connection) = connection {
        if clear {
            let _ = connection.link.send(None);
        }
        connection.link.close();
    }
}

fn set_status(shared: &Shared, status: Status) {
    if let Ok(mut current) = shared.status.lock() {
        *current = status;
    }
}

fn run(shared: &Shared, mut connector: Box<dyn Connector>, retry_min: Duration) {
    let mut connection: Option<Connection> = None;
    // What Discord currently shows, as last confirmed.
    let mut shown: Option<Option<Activity>> = None;
    let mut retry = retry_min;
    let mut retry_at: Option<std::time::Instant> = None;
    let mut warned = false;
    let mut desired = Desired::default();
    loop {
        {
            let Ok(mut guard) = shared.desired.lock() else {
                return;
            };
            loop {
                let due = retry_at.is_some_and(|at| at <= std::time::Instant::now());
                if *guard != desired || due {
                    break;
                }
                let wait = retry_at
                    .map(|at| at.saturating_duration_since(std::time::Instant::now()))
                    .unwrap_or(Duration::from_secs(3600));
                guard = match shared.changed.wait_timeout(guard, wait) {
                    Ok((guard, _)) => guard,
                    Err(_) => return,
                };
            }
            desired = guard.clone();
        }
        if desired.shutdown {
            close(connection.take(), true);
            return;
        }
        let Some(application) = desired.application.clone() else {
            // Disabled: clear what we set and disconnect; never connect.
            close(
                connection.take(),
                shown.as_ref().is_some_and(Option::is_some),
            );
            shown = None;
            retry_at = None;
            set_status(shared, Status::Off);
            continue;
        };
        if connection
            .as_ref()
            .is_some_and(|c| c.application != application)
        {
            close(connection.take(), true);
            shown = None;
        }
        let target = derive(&desired.content, desired.hide_titles);
        if connection.is_none() {
            if retry_at.is_some_and(|at| at > std::time::Instant::now()) {
                continue;
            }
            match connector.connect(&application) {
                Ok(link) => {
                    tracing::info!("Discord Rich Presence connected");
                    connection = Some(Connection { link, application });
                    shown = None;
                    retry = retry_min;
                    retry_at = None;
                    warned = false;
                }
                Err(error) => {
                    if warned {
                        tracing::debug!(%error, "Discord IPC still unavailable");
                    } else {
                        tracing::warn!(%error, "Discord IPC unavailable; retrying with backoff");
                        warned = true;
                    }
                    set_status(shared, Status::Waiting(error));
                    retry_at = Some(std::time::Instant::now() + retry);
                    retry = (retry * 2).min(RETRY_MAX);
                    continue;
                }
            }
        }
        if shown.as_ref() == Some(&target) {
            continue;
        }
        let link = &mut connection.as_mut().expect("connected above").link;
        match link.send(target.as_ref()) {
            Ok(()) => {
                set_status(shared, Status::Shown(target.clone()));
                shown = Some(target);
            }
            Err(error) => {
                set_status(shared, Status::Waiting(error.clone()));
                tracing::warn!(%error, "Discord Rich Presence update failed; reconnecting");
                if let Some(mut dead) = connection.take() {
                    dead.link.close();
                }
                shown = None;
                warned = true;
                retry_at = Some(std::time::Instant::now() + retry);
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::supervisor::{SessionInfo, SessionKind};
    use lotus_session::telemetry::Telemetry;

    fn session(state: SessionState, asset: Option<&str>, playing: bool) -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.state = state;
        telemetry.playing = playing;
        SessionSummary {
            info: SessionInfo {
                id: "s".into(),
                kind: SessionKind::Secondary { parent: None },
                asset: asset.map(str::to_string),
                created_unix_ms: 0,
            },
            telemetry,
        }
    }

    fn resolve(handle: &str) -> Option<(String, String)> {
        // Two different directories, same file name.
        let path = match handle {
            "a/game.3dsx" => "C:/a/game.3dsx",
            "b/game.3dsx" => "C:/b/game.3dsx",
            "local/2-hg.3dsx" => "C:/roms/heartgold.3dsx",
            _ => return None,
        };
        Some((path.into(), path.rsplit('/').next().unwrap().into()))
    }

    fn details(sessions: &[SessionSummary], hide: bool) -> Option<String> {
        derive(&live_content(sessions, resolve), hide).map(|a| a.details)
    }

    #[test]
    fn counts_live_loaded_sessions_only() {
        use SessionState::*;
        assert_eq!(details(&[], false), None);
        let dead = [
            session(Idle, None, false),
            session(Created, Some("a/game.3dsx"), false),
            session(Stopped, Some("a/game.3dsx"), false),
            session(Crashed, Some("a/game.3dsx"), false),
        ];
        assert_eq!(details(&dead, false), None, "nothing live clears");
        // Paused at a breakpoint (not playing) still counts.
        let one = [
            session(Paused, Some("local/2-hg.3dsx"), false),
            dead[0].clone(),
        ];
        assert_eq!(details(&one, false).unwrap(), "Playing heartgold.3dsx...");
        assert_eq!(details(&one, true).unwrap(), "Playing a game");
    }

    #[test]
    fn compares_canonical_content_not_names() {
        use SessionState::*;
        let same = [
            session(Running, Some("a/game.3dsx"), true),
            session(Paused, Some("a/game.3dsx"), false),
        ];
        let activity = derive(&live_content(&same, resolve), false).unwrap();
        assert_eq!(activity.details, "Debugging game.3dsx...");
        assert_eq!(activity.state.as_deref(), Some("2 sessions"));
        let namesakes = [
            session(Running, Some("a/game.3dsx"), true),
            session(Running, Some("b/game.3dsx"), true),
        ];
        assert_eq!(
            details(&namesakes, false).unwrap(),
            "Debugging multiple games"
        );
        assert_eq!(details(&namesakes, true).unwrap(), "Debugging sessions");
    }

    #[test]
    fn names_never_carry_paths_or_control_characters() {
        let content = live_content(
            &[session(
                SessionState::Running,
                Some(r"local\evil\x.3dsx"),
                true,
            )],
            |_| None,
        );
        assert_eq!(content[0].title, "x.3dsx");
        assert_eq!(sanitize("a\nb"), "ab");
        assert_eq!(sanitize(&"x".repeat(200)).chars().count(), 96);
    }

    /// Scripted Discord: `available` gates connects, `fail_sends` makes
    /// the next sends fail as a closed pipe would. Every call is logged.
    #[derive(Default)]
    struct Script {
        available: bool,
        fail_sends: usize,
        log: Vec<String>,
    }

    struct Fake(Arc<Mutex<Script>>);
    struct FakeLink(Arc<Mutex<Script>>);

    impl Connector for Fake {
        fn connect(&mut self, application: &str) -> Result<Box<dyn Link>, String> {
            let mut script = self.0.lock().unwrap();
            script.log.push(format!("connect {application}"));
            if script.available {
                Ok(Box::new(FakeLink(self.0.clone())))
            } else {
                Err("no pipe".into())
            }
        }
    }

    impl Link for FakeLink {
        fn send(&mut self, activity: Option<&Activity>) -> Result<(), String> {
            let mut script = self.0.lock().unwrap();
            let entry = activity.map_or("clear".into(), |a| a.details.clone());
            if script.fail_sends > 0 {
                script.fail_sends -= 1;
                script.log.push(format!("failed {entry}"));
                return Err("pipe closed".into());
            }
            script.log.push(format!("send {entry}"));
            Ok(())
        }
        fn close(&mut self) {
            self.0.lock().unwrap().log.push("close".into());
        }
    }

    fn fake(available: bool) -> (Presence, Arc<Mutex<Script>>) {
        let script = Arc::new(Mutex::new(Script {
            available,
            ..Script::default()
        }));
        let presence =
            Presence::start_with(Box::new(Fake(script.clone())), Duration::from_millis(20));
        (presence, script)
    }

    fn enabled() -> Preferences {
        Preferences {
            discord_presence: true,
            discord_application_id: Some("42".into()),
            ..Preferences::default()
        }
    }

    fn game(name: &str) -> Vec<Content> {
        vec![Content {
            identity: format!("C:/{name}"),
            title: name.into(),
        }]
    }

    fn eventually(presence: &Presence, check: impl Fn(&Status) -> bool) -> Status {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = presence.status();
            if check(&status) {
                return status;
            }
            assert!(std::time::Instant::now() < deadline, "stuck at {status:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn shown(details: Option<&str>) -> impl Fn(&Status) -> bool + '_ {
        move |status| matches!(status, Status::Shown(a) if a.as_ref().map(|a| a.details.as_str()) == details)
    }

    fn log(script: &Arc<Mutex<Script>>) -> Vec<String> {
        script.lock().unwrap().log.clone()
    }

    #[test]
    fn disabled_never_connects() {
        let (presence, script) = fake(true);
        presence.publish(game("a.3dsx"));
        presence.configure(&Preferences::default());
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(presence.status(), Status::Off);
        drop(presence);
        assert!(log(&script).is_empty(), "{:?}", log(&script));
    }

    #[test]
    fn waits_with_backoff_then_connects_when_discord_starts() {
        let (presence, script) = fake(false);
        presence.configure(&enabled());
        presence.publish(game("a.3dsx"));
        eventually(&presence, |s| matches!(s, Status::Waiting(_)));
        // 20, 40, 80, 160 ms: at most 5 attempts in 300 ms, not a busy loop.
        std::thread::sleep(Duration::from_millis(300));
        let attempts = log(&script).len();
        assert!((2..=5).contains(&attempts), "{attempts} connects");
        script.lock().unwrap().available = true;
        eventually(&presence, shown(Some("Playing a.3dsx...")));
    }

    #[test]
    fn identical_updates_are_sent_once() {
        let (presence, script) = fake(true);
        presence.configure(&enabled());
        presence.publish(game("a.3dsx"));
        eventually(&presence, shown(Some("Playing a.3dsx...")));
        for _ in 0..50 {
            presence.publish(game("a.3dsx"));
            presence.configure(&enabled());
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(log(&script), ["connect 42", "send Playing a.3dsx..."]);
    }

    #[test]
    fn reconnects_after_discord_restarts() {
        let (presence, script) = fake(true);
        presence.configure(&enabled());
        presence.publish(game("a.3dsx"));
        eventually(&presence, shown(Some("Playing a.3dsx...")));
        script.lock().unwrap().fail_sends = 1;
        presence.publish(game("b.3dsx"));
        eventually(&presence, shown(Some("Playing b.3dsx...")));
        assert_eq!(
            log(&script),
            [
                "connect 42",
                "send Playing a.3dsx...",
                "failed Playing b.3dsx...",
                "close",
                "connect 42",
                "send Playing b.3dsx...",
            ]
        );
    }

    #[test]
    fn last_session_closing_disabling_and_shutdown_clear() {
        let (presence, script) = fake(true);
        presence.configure(&enabled());
        presence.publish(game("a.3dsx"));
        eventually(&presence, shown(Some("Playing a.3dsx...")));
        presence.publish(Vec::new());
        eventually(&presence, shown(None));
        presence.publish(game("a.3dsx"));
        eventually(&presence, shown(Some("Playing a.3dsx...")));
        presence.configure(&Preferences::default());
        eventually(&presence, |s| *s == Status::Off);
        presence.configure(&enabled());
        eventually(&presence, shown(Some("Playing a.3dsx...")));
        drop(presence);
        assert_eq!(
            log(&script),
            [
                "connect 42",
                "send Playing a.3dsx...",
                "send clear",
                "send Playing a.3dsx...",
                "send clear",
                "close",
                "connect 42",
                "send Playing a.3dsx...",
                "send clear",
                "close",
            ]
        );
    }

    #[test]
    fn application_ids_must_be_snowflakes() {
        let mut preferences = Preferences {
            discord_application_id: Some("123456789012345678".into()),
            ..Preferences::default()
        };
        assert!(application_id(&preferences).is_some());
        preferences.discord_application_id = Some("12; rm".into());
        assert!(application_id(&preferences).is_none());
        preferences.discord_application_id = None;
        assert!(application_id(&preferences).is_some(), "built-in default");
    }
}
