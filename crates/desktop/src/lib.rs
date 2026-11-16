//! Desktop application state.
//!
//! The GPUI layer owns presentation only. It never reaches into the native
//! worker: it renders immutable snapshots that the session supervisor
//! publishes.

use lotus_session::{SessionId, SessionState};
use serde::{Deserialize, Serialize};

/// Summary of one emulator instance, as shown in the instance list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session identifier.
    pub id: String,
    /// Current lifecycle state.
    pub state: SessionState,
    /// Console model the session emulates.
    pub model: String,
    /// Render backend the session requested.
    pub renderer: String,
}

/// State of the desktop application.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LotusUiState {
    /// Sessions known to the supervisor.
    pub sessions: Vec<SessionSummary>,
    /// Identifier of the selected session.
    pub selected: Option<String>,
    /// Worker protocol version the UI was built against.
    pub protocol_version: String,
}

impl LotusUiState {
    /// Create the initial state.
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: None,
            protocol_version: lotus_protocol::protocol_id(),
        }
    }

    /// Summary of a session.
    pub fn summary(
        id: SessionId,
        state: SessionState,
        model: impl Into<String>,
        renderer: impl Into<String>,
    ) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            state,
            model: model.into(),
            renderer: renderer.into(),
        }
    }

    /// Select a session by identifier.
    pub fn select(&mut self, id: &str) {
        if self.sessions.iter().any(|session| session.id == id) {
            self.selected = Some(id.to_string());
        }
    }

    /// Summary of the selected session.
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        let selected = self.selected.as_ref()?;
        self.sessions.iter().find(|session| &session.id == selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_requires_a_known_session() {
        let mut state = LotusUiState::new();
        state.select("session-00000000000000000000000000000001");
        assert!(state.selected.is_none());

        state.sessions.push(SessionSummary {
            id: "session-1".to_string(),
            state: SessionState::Idle,
            model: "old3ds".to_string(),
            renderer: "software".to_string(),
        });
        state.select("session-1");
        assert_eq!(state.selected.as_deref(), Some("session-1"));
        assert!(state.selected_session().is_some());
    }

    #[test]
    fn protocol_version_is_reported() {
        assert_eq!(LotusUiState::new().protocol_version, "1.0");
    }
}
pub mod screen_geometry;
