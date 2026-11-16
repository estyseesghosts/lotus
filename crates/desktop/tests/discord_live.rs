//! Rich Presence against a real, running Discord client (issue #18).
//!
//! Ignored by default: it changes the signed-in user's visible Discord status.
//! `cargo test -p lotus-desktop --test discord_live -- --ignored --nocapture`
//! `LOTUS_PRESENCE_HOLD_SECONDS` (default 10) keeps each state up for a person
//! to check the profile. Passing means Discord accepted every SET_ACTIVITY
//! (the presence thread reads each reply and reports ERROR events).
use std::time::{Duration, Instant};

use lotus_desktop::preferences::Preferences;
use lotus_desktop::presence::{Activity, Content, Presence, Status};

fn wait_for(presence: &Presence, expected: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = presence.status();
        if let Status::Shown(shown) = &status {
            if shown.as_ref().map(|a: &Activity| a.details.as_str()) == expected {
                println!("Discord confirmed: {expected:?}");
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, have {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn content(identity: &str, title: &str) -> Content {
    Content {
        identity: identity.into(),
        title: title.into(),
    }
}

#[test]
#[ignore = "changes the real Discord status of the signed-in user"]
fn real_discord_accepts_every_state() {
    let hold = Duration::from_secs(
        std::env::var("LOTUS_PRESENCE_HOLD_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10),
    );
    let presence = Presence::start();
    presence.configure(&Preferences {
        discord_presence: true,
        ..Preferences::default()
    });
    wait_for(&presence, None);
    presence.publish(vec![content("C:/a/game.3dsx", "game.3dsx")]);
    wait_for(&presence, Some("Playing game.3dsx..."));
    std::thread::sleep(hold);
    presence.publish(vec![
        content("C:/a/game.3dsx", "game.3dsx"),
        content("C:/a/game.3dsx", "game.3dsx"),
    ]);
    wait_for(&presence, Some("Debugging game.3dsx..."));
    std::thread::sleep(hold);
    presence.publish(vec![
        content("C:/a/game.3dsx", "game.3dsx"),
        content("C:/b/game.3dsx", "game.3dsx"),
    ]);
    wait_for(&presence, Some("Debugging multiple games"));
    std::thread::sleep(hold);
    presence.configure(&Preferences {
        discord_presence: true,
        discord_hide_titles: true,
        ..Preferences::default()
    });
    wait_for(&presence, Some("Debugging sessions"));
    presence.publish(Vec::new());
    wait_for(&presence, None);
    // Disabling disconnects; dropping afterwards must not reconnect.
    presence.configure(&Preferences::default());
    let deadline = Instant::now() + Duration::from_secs(5);
    while presence.status() != Status::Off {
        assert!(Instant::now() < deadline, "{:?}", presence.status());
        std::thread::sleep(Duration::from_millis(100));
    }
}
