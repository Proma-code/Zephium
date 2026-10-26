//! Assistant conversations: one opaque body each in the profile database, newest
//! first, held to a count.

use super::*;

use zephium_core::ports::store::{
    ChatSummary, MAX_CHATS, MAX_CHAT_BODY_BYTES, MAX_CHAT_ID_BYTES, MAX_CHAT_SPACE_BYTES,
    MAX_CHAT_TITLE_BYTES,
};

pub(crate) fn valid_chat_id(id: &str) -> bool {
    (1..=MAX_CHAT_ID_BYTES).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

pub(crate) fn valid_chat(id: &str, space: &str, title: &str, body: &str) -> bool {
    valid_chat_id(id)
        && space.len() <= MAX_CHAT_SPACE_BYTES
        && title.len() <= MAX_CHAT_TITLE_BYTES
        && body.len() <= MAX_CHAT_BODY_BYTES
}

impl Hub {
    fn chats_available(&self, profile: ProfileId) -> bool {
        self.registry.contains(&profile)
            && !self.degraded_profiles.contains(&profile)
            && self.recovery_required.is_none()
    }

    pub(crate) fn chats(&mut self, profile: ProfileId, limit: u32) -> Vec<ChatSummary> {
        let limit = limit.min(MAX_CHATS as u32);
        if limit == 0 || !self.chats_available(profile) {
            return Vec::new();
        }
        self.profile_conn(profile)
            .and_then(|conn| {
                conn.prepare_cached(
                    "SELECT id, space, title, updated_ms FROM chats
                     ORDER BY updated_ms DESC, id DESC LIMIT ?1",
                )?
                .query_map([limit], |row| {
                    Ok(ChatSummary {
                        id: row.get(0)?,
                        space: row.get(1)?,
                        title: row.get(2)?,
                        updated_ms: row.get(3)?,
                    })
                })?
                .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn chat(&mut self, profile: ProfileId, id: &str) -> Option<String> {
        if !valid_chat_id(id) || !self.chats_available(profile) {
            return None;
        }
        self.profile_conn(profile)
            .and_then(|conn| {
                conn.prepare_cached("SELECT body FROM chats WHERE id = ?1")?
                    .query_row([id], |row| row.get(0))
                    .optional()
            })
            .ok()
            .flatten()
    }

    /// Replaces the conversation with the same id, or adds one and drops the
    /// oldest past `MAX_CHATS` in the same transaction. A conversation older
    /// than every one kept is not stored.
    pub(crate) fn save_chat(
        &mut self,
        profile: ProfileId,
        id: &str,
        space: &str,
        title: &str,
        updated_ms: i64,
        body: &str,
    ) -> bool {
        if !valid_chat(id, space, title, body) || !self.chats_available(profile) {
            return false;
        }
        let result = self.profile_conn(profile).and_then(|conn| {
            let tx = conn.transaction()?;
            tx.prepare_cached(
                "INSERT INTO chats(id, space, title, updated_ms, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                     space = excluded.space, title = excluded.title,
                     updated_ms = excluded.updated_ms, body = excluded.body",
            )?
            .execute(params![id, space, title, updated_ms, body])?;
            tx.prepare_cached(
                "DELETE FROM codex_conversations WHERE conversation IN (
                     SELECT id FROM chats
                     ORDER BY updated_ms DESC, id DESC
                     LIMIT -1 OFFSET ?1
                 )",
            )?
            .execute([MAX_CHATS as i64])?;
            tx.prepare_cached(
                "DELETE FROM chats WHERE id IN (
                     SELECT id FROM chats
                     ORDER BY updated_ms DESC, id DESC
                     LIMIT -1 OFFSET ?1
                 )",
            )?
            .execute([MAX_CHATS as i64])?;
            let kept = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)")?
                .query_row([id], |row| row.get::<_, bool>(0))?;
            tx.commit()?;
            Ok(kept)
        });
        result.unwrap_or_else(|e| {
            eprintln!("store: save_chat failed for profile {profile}: {e}");
            false
        })
    }

    pub(crate) fn delete_chat(&mut self, profile: ProfileId, id: &str) -> bool {
        if !valid_chat_id(id) || !self.chats_available(profile) {
            return false;
        }
        let result = self.profile_conn(profile).and_then(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM codex_conversations WHERE conversation = ?1",
                [id],
            )?;
            let removed = tx
                .prepare_cached("DELETE FROM chats WHERE id = ?1")?
                .execute([id])?;
            tx.commit()?;
            Ok(removed)
        });
        match result {
            Ok(removed) => removed > 0,
            Err(e) => {
                eprintln!("store: delete_chat failed for profile {profile}: {e}");
                false
            }
        }
    }

    /// Removes conversations updated at or after `since` (Unix seconds), or
    /// all of them when it is absent.
    pub(crate) fn clear_chats(&mut self, profile: ProfileId, since: Option<i64>) -> u32 {
        if !self.chats_available(profile) {
            return 0;
        }
        let result = self.profile_conn(profile).and_then(|conn| {
            let tx = conn.transaction()?;
            let removed = match since {
                Some(since) => {
                    let since = since.saturating_mul(1000);
                    tx.execute("DELETE FROM codex_conversations WHERE conversation IN (SELECT id FROM chats WHERE updated_ms >= ?1)", [since])?;
                    tx.execute("DELETE FROM chats WHERE updated_ms >= ?1", [since])?
                },
                None => {
                    tx.execute("DELETE FROM codex_conversations", [])?;
                    tx.execute("DELETE FROM chats", [])?
                },
            };
            tx.commit()?;
            Ok(removed)
        });
        match result {
            Ok(removed) => u32::try_from(removed).unwrap_or(u32::MAX),
            Err(e) => {
                eprintln!("store: clear_chats failed for profile {profile}: {e}");
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_hub() -> (Hub, ProfileId) {
        let mut hub = Hub::in_memory().unwrap();
        let profile = ProfileId::from(1);
        hub.registry.insert(profile);
        (hub, profile)
    }

    fn save(hub: &mut Hub, profile: ProfileId, id: &str, updated_ms: i64) -> bool {
        hub.save_chat(profile, id, "space", "title", updated_ms, "{}")
    }

    fn ids(hub: &mut Hub, profile: ProfileId) -> Vec<String> {
        hub.chats(profile, u32::MAX)
            .into_iter()
            .map(|chat| chat.id)
            .collect()
    }

    #[test]
    fn a_saved_conversation_reads_back_and_a_resave_replaces_it() {
        let (mut hub, profile) = registered_hub();
        assert!(hub.save_chat(
            profile,
            "chat-1",
            "space-a",
            "First",
            10,
            r#"{"turns":[1]}"#
        ));
        assert!(hub.save_chat(profile, "chat-2", "space-b", "Second", 20, "{}"));

        assert_eq!(
            hub.chats(profile, 10),
            vec![
                ChatSummary {
                    id: "chat-2".into(),
                    space: "space-b".into(),
                    title: "Second".into(),
                    updated_ms: 20,
                },
                ChatSummary {
                    id: "chat-1".into(),
                    space: "space-a".into(),
                    title: "First".into(),
                    updated_ms: 10,
                },
            ]
        );
        assert_eq!(
            hub.chat(profile, "chat-1").as_deref(),
            Some(r#"{"turns":[1]}"#)
        );
        assert_eq!(hub.chat(profile, "missing"), None);

        assert!(hub.save_chat(
            profile,
            "chat-1",
            "space-c",
            "Renamed",
            30,
            r#"{"turns":[1,2]}"#
        ));
        let listed = hub.chats(profile, 10);
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0],
            ChatSummary {
                id: "chat-1".into(),
                space: "space-c".into(),
                title: "Renamed".into(),
                updated_ms: 30,
            }
        );
        assert_eq!(
            hub.chat(profile, "chat-1").as_deref(),
            Some(r#"{"turns":[1,2]}"#)
        );
    }

    #[test]
    fn listing_is_newest_first_with_a_stable_tie_and_honors_the_limit() {
        let (mut hub, profile) = registered_hub();
        for (id, updated_ms) in [("a", 5), ("c", 7), ("b", 7), ("d", 1)] {
            assert!(save(&mut hub, profile, id, updated_ms));
        }
        assert_eq!(ids(&mut hub, profile), ["c", "b", "a", "d"]);
        assert_eq!(hub.chats(profile, 2).len(), 2);
        assert!(hub.chats(profile, 0).is_empty());
    }

    #[test]
    fn values_past_a_bound_are_refused_and_the_bounds_themselves_are_kept() {
        let (mut hub, profile) = registered_hub();
        let id = "a".repeat(MAX_CHAT_ID_BYTES);
        let space = "s".repeat(MAX_CHAT_SPACE_BYTES);
        let title = "t".repeat(MAX_CHAT_TITLE_BYTES);
        let body = "b".repeat(MAX_CHAT_BODY_BYTES);
        assert!(hub.save_chat(profile, &id, &space, &title, 1, &body));
        assert!(hub.save_chat(profile, "Z-9", "", "", 2, ""));
        assert_eq!(hub.chat(profile, &id).as_deref(), Some(body.as_str()));
        assert_eq!(hub.chat(profile, "Z-9").as_deref(), Some(""));

        let mut refused = |id: &str, space: &str, title: &str, body: &str| {
            assert!(!hub.save_chat(profile, id, space, title, 3, body), "{id:?}");
        };
        refused("", "s", "t", "b");
        refused(&"a".repeat(MAX_CHAT_ID_BYTES + 1), "s", "t", "b");
        for id in ["has space", "under_score", "../up", "caf\u{e9}", "nul\0"] {
            refused(id, "s", "t", "b");
        }
        refused("ok", &"s".repeat(MAX_CHAT_SPACE_BYTES + 1), "t", "b");
        refused("ok", "s", &"t".repeat(MAX_CHAT_TITLE_BYTES + 1), "b");
        refused(
            "ok",
            "s",
            &"\u{e9}".repeat(MAX_CHAT_TITLE_BYTES / 2 + 1),
            "b",
        );
        refused("ok", "s", "t", &"b".repeat(MAX_CHAT_BODY_BYTES + 1));
        assert_eq!(ids(&mut hub, profile), ["Z-9", id.as_str()]);
        assert_eq!(hub.chat(profile, "../up"), None);
        assert!(!hub.delete_chat(profile, "../up"));
    }

    #[test]
    fn saving_past_the_cap_drops_the_oldest_in_the_same_write() {
        let (mut hub, profile) = registered_hub();
        for index in 0..MAX_CHATS as i64 {
            assert!(save(
                &mut hub,
                profile,
                &format!("chat-{index:03}"),
                100 + index
            ));
        }
        assert_eq!(hub.chats(profile, u32::MAX).len(), MAX_CHATS);

        assert!(save(&mut hub, profile, "newest", 1_000));
        let kept = ids(&mut hub, profile);
        assert_eq!(kept.len(), MAX_CHATS);
        assert_eq!(kept[0], "newest");
        assert!(!kept.contains(&"chat-000".to_owned()));
        assert!(kept.contains(&"chat-001".to_owned()));

        // Updating a kept conversation grows nothing and drops nothing.
        assert!(save(&mut hub, profile, "chat-001", 2_000));
        assert_eq!(ids(&mut hub, profile).len(), MAX_CHATS);
        assert_eq!(ids(&mut hub, profile)[0], "chat-001");

        // One older than everything kept is not stored, and costs nothing.
        assert!(!save(&mut hub, profile, "ancient", 1));
        assert_eq!(hub.chat(profile, "ancient"), None);
        assert!(hub.chat(profile, "chat-002").is_some());
        assert_eq!(ids(&mut hub, profile).len(), MAX_CHATS);
    }

    #[test]
    fn deleting_reports_whether_a_conversation_went() {
        let (mut hub, profile) = registered_hub();
        assert!(save(&mut hub, profile, "gone", 1));
        assert!(save(&mut hub, profile, "kept", 2));
        assert!(hub.delete_chat(profile, "gone"));
        assert!(!hub.delete_chat(profile, "gone"));
        assert_eq!(ids(&mut hub, profile), ["kept"]);
    }

    #[test]
    fn clearing_removes_conversations_from_a_second_on_or_all_of_them() {
        let (mut hub, profile) = registered_hub();
        for (id, updated_ms) in [("old", 1_000), ("edge", 5_000), ("new", 9_999)] {
            assert!(save(&mut hub, profile, id, updated_ms));
        }
        // `since` is Unix seconds, as for history; updates are milliseconds.
        assert_eq!(hub.clear_chats(profile, Some(5)), 2);
        assert_eq!(ids(&mut hub, profile), ["old"]);
        assert_eq!(hub.clear_chats(profile, Some(5)), 0);

        assert!(save(&mut hub, profile, "again", 2_000));
        assert_eq!(hub.clear_chats(profile, None), 2);
        assert!(ids(&mut hub, profile).is_empty());
        assert_eq!(hub.clear_chats(profile, Some(i64::MAX)), 0);
    }

    #[test]
    fn clearing_history_leaves_conversations_alone() {
        let (mut hub, profile) = registered_hub();
        assert!(save(&mut hub, profile, "chat", 1));
        hub.record_visit(profile, "https://example.com/", "Example");
        assert_eq!(hub.clear_history(profile, None), 1);
        assert_eq!(ids(&mut hub, profile), ["chat"]);
    }

    #[test]
    fn conversations_belong_to_their_profile_and_only_registered_ones_store() {
        let (mut hub, profile) = registered_hub();
        let other = ProfileId::from(2);
        hub.registry.insert(other);
        let stranger = ProfileId::from(3);

        assert!(save(&mut hub, profile, "mine", 1));
        assert!(!save(&mut hub, stranger, "mine", 1));
        assert!(hub.chats(other, 10).is_empty());
        assert_eq!(hub.chat(other, "mine"), None);
        assert!(!hub.delete_chat(other, "mine"));
        assert_eq!(hub.clear_chats(other, None), 0);
        assert_eq!(ids(&mut hub, profile), ["mine"]);

        assert!(hub.chats(stranger, 10).is_empty());
        assert_eq!(hub.chat(stranger, "mine"), None);
        assert_eq!(hub.clear_chats(stranger, None), 0);
    }

    #[test]
    fn a_degraded_profile_stores_and_reads_nothing() {
        let (mut hub, profile) = registered_hub();
        assert!(save(&mut hub, profile, "chat", 1));
        hub.degraded_profiles.insert(profile);
        assert!(!save(&mut hub, profile, "more", 2));
        assert!(hub.chats(profile, 10).is_empty());
        assert_eq!(hub.chat(profile, "chat"), None);
        assert!(!hub.delete_chat(profile, "chat"));
        assert_eq!(hub.clear_chats(profile, None), 0);
    }
}
