//! Sidebar structure: primaries are top-level, secondaries nest under the
//! card they were copied from, and parentless secondaries stand alone.
//! A card whose primary is not running still heads its secondaries.
use std::collections::BTreeMap;

use lotus_session::supervisor::{SessionInfo, SessionKind, SessionSummary};

#[derive(Debug, Clone)]
pub enum TreeNode {
    Card {
        card: String,
        primary: Option<SessionSummary>,
        secondaries: Vec<SessionSummary>,
    },
    Detached(SessionSummary),
}

/// Cards in name order, then detached secondaries; children by creation time.
#[must_use]
pub fn session_tree(summaries: &[SessionSummary]) -> Vec<TreeNode> {
    let mut cards: BTreeMap<String, (Option<SessionSummary>, Vec<SessionSummary>)> =
        BTreeMap::new();
    let mut detached = Vec::new();
    for summary in summaries {
        match &summary.info.kind {
            SessionKind::Primary { card } => {
                cards.entry(card.clone()).or_default().0 = Some(summary.clone());
            }
            SessionKind::Secondary {
                parent: Some(parent),
            } => cards
                .entry(parent.clone())
                .or_default()
                .1
                .push(summary.clone()),
            SessionKind::Secondary { parent: None } => detached.push(summary.clone()),
        }
    }
    let mut nodes = Vec::new();
    for (card, (primary, mut secondaries)) in cards {
        secondaries.sort_by_key(|summary| summary.info.created_unix_ms);
        nodes.push(TreeNode::Card {
            card,
            primary,
            secondaries,
        });
    }
    detached.sort_by_key(|summary| summary.info.created_unix_ms);
    nodes.extend(detached.into_iter().map(TreeNode::Detached));
    nodes
}

/// Display name: the card for primaries, the content for secondaries.
#[must_use]
pub fn session_label(info: &SessionInfo) -> String {
    match &info.kind {
        SessionKind::Primary { card } => card.clone(),
        SessionKind::Secondary { .. } => info
            .asset
            .as_deref()
            .map(asset_label)
            .unwrap_or_else(|| "empty".to_string()),
    }
}

/// Short, stable tag for telling sibling sessions apart. Session IDs begin with
/// a little-endian process counter, so their leading hex is not distinctive;
/// six hex digits of an FNV-1a hash of the whole ID are.
#[must_use]
pub fn short_id(id: &str) -> String {
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{:06x}", hash & 0xff_ffff)
}

/// File stem of an asset handle such as `builds/demo.3dsx`.
#[must_use]
pub fn asset_label(handle: &str) -> String {
    let name = handle.rsplit('/').next().unwrap_or(handle);
    name.strip_suffix(".3dsx")
        .or_else(|| name.strip_suffix(".3DSX"))
        .unwrap_or(name)
        .to_string()
}

/// First `card-N` (or `default`) not already taken.
#[must_use]
pub fn next_card_name(existing: &[String]) -> String {
    if !existing.iter().any(|name| name == "default") {
        return "default".to_string();
    }
    (2..)
        .map(|index| format!("card-{index}"))
        .find(|name| !existing.contains(name))
        .expect("unbounded candidate names")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::telemetry::Telemetry;

    fn summary(id: &str, kind: SessionKind, created: u64) -> SessionSummary {
        SessionSummary {
            info: SessionInfo {
                id: id.into(),
                kind,
                asset: Some("builds/demo.3dsx".into()),
                created_unix_ms: created,
            },
            telemetry: Telemetry::new(),
        }
    }

    #[test]
    fn secondaries_nest_under_their_card_even_when_primary_is_offline() {
        let summaries = [
            summary(
                "c2",
                SessionKind::Secondary {
                    parent: Some("dev".into()),
                },
                3,
            ),
            summary("p1", SessionKind::Primary { card: "dev".into() }, 1),
            summary(
                "c1",
                SessionKind::Secondary {
                    parent: Some("dev".into()),
                },
                2,
            ),
            summary("o1", SessionKind::Secondary { parent: None }, 4),
            summary(
                "c3",
                SessionKind::Secondary {
                    parent: Some("alt".into()),
                },
                5,
            ),
        ];
        let tree = session_tree(&summaries);
        assert_eq!(tree.len(), 3);
        let TreeNode::Card {
            card,
            primary,
            secondaries,
        } = &tree[0]
        else {
            panic!("cards first");
        };
        assert_eq!(card, "alt");
        assert!(primary.is_none(), "offline card still heads its copies");
        assert_eq!(secondaries.len(), 1);
        let TreeNode::Card {
            primary,
            secondaries,
            ..
        } = &tree[1]
        else {
            panic!("dev card");
        };
        assert_eq!(primary.as_ref().map(|p| p.info.id.as_str()), Some("p1"));
        let order: Vec<&str> = secondaries.iter().map(|s| s.info.id.as_str()).collect();
        assert_eq!(order, ["c1", "c2"]);
        assert!(matches!(&tree[2], TreeNode::Detached(s) if s.info.id == "o1"));
    }

    #[test]
    fn labels_and_card_names() {
        assert_eq!(asset_label("builds/demo.3dsx"), "demo");
        assert_eq!(asset_label("fixture"), "fixture");
        let first = short_id("03000000000000001122334455667788");
        assert_eq!(first.len(), 6);
        assert_eq!(first, short_id("03000000000000001122334455667788"));
        assert_ne!(first, short_id("04000000000000001122334455667788"));
        assert_eq!(next_card_name(&[]), "default");
        assert_eq!(
            next_card_name(&["default".into(), "card-2".into()]),
            "card-3"
        );
    }
}
