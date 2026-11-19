//! Lotus-owned projection into generic source nodes (#130/#138).
//! Row IDs are navigation identity, separate from current worker identity.
//! Callbacks must resolve captured SessionTarget against the latest snapshot.
use crate::session_tree::{self, TreeNode};
use lotus_session::supervisor::{SessionKind, SessionSummary};
use silvestris::core::source_list::SourceNode;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTarget {
    pub id: String,
    pub created_unix_ms: u64,
    pub epoch: u64,
}
impl SessionTarget {
    fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            id: summary.info.id.clone(),
            created_unix_ms: summary.info.created_unix_ms,
            epoch: summary.telemetry.epoch,
        }
    }
    /// Reject missing/replaced sessions and responses from a previous guest epoch.
    /// Availability/permission is command-specific and must also be checked.
    pub fn resolve<'a>(&self, sessions: &'a [SessionSummary]) -> Option<&'a SessionSummary> {
        sessions.iter().find(|s| {
            s.info.id == self.id
                && s.info.created_unix_ms == self.created_unix_ms
                && s.telemetry.epoch == self.epoch
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Card {
        card: String,
        primary: Option<SessionTarget>,
    },
    Session(SessionTarget),
}
#[derive(Clone, Debug, Default)]
pub struct Navigation {
    pub nodes: Vec<SourceNode>,
    pub targets: BTreeMap<String, Target>,
}
pub fn card_row(card: &str) -> String {
    format!("card:{card}")
}
pub fn session_row(id: &str) -> String {
    format!("session:{id}")
}

impl Navigation {
    pub fn project(cards: &[String], sessions: &[SessionSummary]) -> Self {
        let mut result = Self::default();
        let mut card_nodes = BTreeMap::new();
        let mut detached = Vec::new();
        for card in cards {
            let mut node = SourceNode::new(card_row(card), card.clone());
            node.selectable = false;
            node.icon = Some("card.offline".into());
            node.badge = Some("offline".into());
            result.targets.insert(
                node.id.clone(),
                Target::Card {
                    card: card.clone(),
                    primary: None,
                },
            );
            card_nodes.insert(card.clone(), node);
        }
        for node in session_tree::session_tree(sessions) {
            match node {
                TreeNode::Card {
                    card,
                    primary,
                    secondaries,
                } => {
                    let mut node = SourceNode::new(card_row(&card), card.clone());
                    node.selectable = primary.is_some();
                    node.icon = Some(if primary.is_some() {
                        format!("state:{}", node.id)
                    } else {
                        "card.offline".into()
                    });
                    node.badge = Some(
                        primary
                            .as_ref()
                            .map(session_tree::session_detail)
                            .unwrap_or_else(|| "offline".into()),
                    );
                    result.targets.insert(
                        node.id.clone(),
                        Target::Card {
                            card: card.clone(),
                            primary: primary.as_ref().map(SessionTarget::from_summary),
                        },
                    );
                    node.children = secondaries.iter().map(|s| result.secondary(s)).collect();
                    card_nodes.insert(card, node);
                }
                TreeNode::Detached(summary) => detached.push(result.secondary(&summary)),
            }
        }
        if !card_nodes.is_empty() {
            result.nodes.push(SourceNode::section(
                "group:cards",
                "Cards",
                card_nodes.into_values().collect(),
            ));
        }
        if !detached.is_empty() {
            result
                .nodes
                .push(SourceNode::section("group:sessions", "Sessions", detached));
        }
        result
    }

    fn secondary(&mut self, summary: &SessionSummary) -> SourceNode {
        let mut node = SourceNode::new(
            session_row(&summary.info.id),
            format!(
                "{} · {}",
                session_tree::session_label(&summary.info),
                session_tree::short_id(&summary.info.id)
            ),
        );
        node.icon = Some(format!("state:{}", node.id));
        node.badge = Some(session_tree::session_detail(summary));
        self.targets.insert(
            node.id.clone(),
            Target::Session(SessionTarget::from_summary(summary)),
        );
        node
    }

    pub fn selected_row(&self, selected: &str, sessions: &[SessionSummary]) -> Option<String> {
        let summary = sessions.iter().find(|s| s.info.id == selected)?;
        let row = match &summary.info.kind {
            SessionKind::Primary { card } => card_row(card),
            SessionKind::Secondary { .. } => session_row(selected),
        };
        self.targets.contains_key(&row).then_some(row)
    }

    /// Card selection follows its current primary; never infer a target from
    /// display text. Offline cards remain expandable but nonselectable.
    pub fn selection<'a>(
        &self,
        row: &str,
        current: &'a [SessionSummary],
    ) -> Option<&'a SessionSummary> {
        let target = match self.targets.get(row)? {
            Target::Card { primary, .. } => primary.as_ref()?,
            Target::Session(target) => target,
        };
        // A reset epoch does not change the person's session selection intent.
        current
            .iter()
            .find(|s| s.info.id == target.id && s.info.created_unix_ms == target.created_unix_ms)
    }

    pub fn expandable_rows(&self) -> BTreeSet<String> {
        fn walk(nodes: &[SourceNode], out: &mut BTreeSet<String>) {
            for n in nodes {
                if !n.section && !n.children.is_empty() {
                    out.insert(n.id.clone());
                }
                walk(&n.children, out);
            }
        }
        let mut out = BTreeSet::new();
        walk(&self.nodes, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};
    use silvestris::core::source_list::SourceTree;
    fn summary(id: &str, kind: SessionKind, created: u64) -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.epoch = 1;
        SessionSummary {
            info: SessionInfo {
                id: id.into(),
                kind,
                asset: Some("fixtures/demo.3dsx".into()),
                created_unix_ms: created,
            },
            telemetry,
        }
    }
    #[test]
    fn saved_offline_cards_and_nested_copies_have_stable_separate_targets() {
        let sessions = vec![
            summary("p", SessionKind::Primary { card: "dev".into() }, 1),
            summary(
                "late",
                SessionKind::Secondary {
                    parent: Some("dev".into()),
                },
                3,
            ),
            summary(
                "early",
                SessionKind::Secondary {
                    parent: Some("dev".into()),
                },
                2,
            ),
            summary(
                "copy",
                SessionKind::Secondary {
                    parent: Some("offline".into()),
                },
                4,
            ),
            summary("detached", SessionKind::Secondary { parent: None }, 5),
        ];
        let nav = Navigation::project(&["saved".into(), "dev".into()], &sessions);
        let cards = &nav.nodes[0].children;
        assert_eq!(
            cards.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            ["dev", "offline", "saved"]
        );
        assert_eq!(
            cards[0]
                .children
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            ["session:early", "session:late"]
        );
        assert!(!cards[1].selectable);
        assert!(!cards[2].selectable);
        assert_eq!(
            nav.selected_row("p", &sessions).as_deref(),
            Some("card:dev")
        );
        assert_eq!(
            nav.selected_row("copy", &sessions).as_deref(),
            Some("session:copy")
        );
        assert_eq!(nav.selection("card:dev", &sessions).unwrap().info.id, "p");
        assert!(nav.selection("card:saved", &sessions).is_none());
        let mut tree = SourceTree::default();
        tree.replace(nav.nodes.clone()).unwrap();
        assert!(tree.expand("card:offline", true));
        assert!(!tree.select("card:offline"));
        assert!(tree.select("session:copy"));
        assert!(nav.expandable_rows().contains("card:offline"));
    }
    #[test]
    fn stale_worker_epoch_and_reused_session_identity_are_refused() {
        let first = summary("worker", SessionKind::Secondary { parent: None }, 1);
        let nav = Navigation::project(&[], std::slice::from_ref(&first));
        let Target::Session(captured) = nav.targets.get("session:worker").unwrap() else {
            panic!("session target");
        };
        assert!(captured.resolve(std::slice::from_ref(&first)).is_some());
        let mut reset = first.clone();
        reset.telemetry.epoch += 1;
        assert!(captured.resolve(std::slice::from_ref(&reset)).is_none());
        assert!(
            nav.selection("session:worker", &[reset]).is_some(),
            "reset preserves session-selection intent"
        );
        let mut replaced = first.clone();
        replaced.info.created_unix_ms += 1;
        assert!(captured.resolve(std::slice::from_ref(&replaced)).is_none());
        assert!(nav.selection("session:worker", &[replaced]).is_none());
        assert!(captured.resolve(&[]).is_none());
    }
    #[test]
    fn card_row_survives_primary_replacement_without_retargeting_old_actions() {
        let old = summary("old", SessionKind::Primary { card: "dev".into() }, 1);
        let new = summary("new", SessionKind::Primary { card: "dev".into() }, 2);
        let old_nav = Navigation::project(&[], &[old]);
        let new_nav = Navigation::project(&[], std::slice::from_ref(&new));
        assert_eq!(
            old_nav.nodes[0].children[0].id,
            new_nav.nodes[0].children[0].id
        );
        assert!(old_nav
            .selection("card:dev", std::slice::from_ref(&new))
            .is_none());
        assert_eq!(
            new_nav.selection("card:dev", &[new]).unwrap().info.id,
            "new"
        );
        let special = Navigation::project(
            &["group:cards".into(), "session:x".into()],
            &[summary(
                "card:session:x",
                SessionKind::Secondary { parent: None },
                3,
            )],
        );
        let mut tree = SourceTree::default();
        tree.replace(special.nodes).unwrap();
    }
}
