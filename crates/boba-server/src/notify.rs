//! Notifications: per-user inbox entries, persisted and pushed to that user's sockets only.

use boba_core::event::{Entity, ServerMessage};
use boba_core::team::{Notification, NotificationKind};
use boba_core::Id;
use chrono::Utc;

use crate::db::new_id;
use crate::state::AppState;

/// What a notification points at, and who caused it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Target {
    pub ticket: Option<Id>,
    pub comment: Option<Id>,
    pub run: Option<Id>,
    pub job: Option<Id>,
    pub actor_user: Option<Id>,
    pub actor_agent: Option<Id>,
}

impl AppState {
    /// Notify each of `users` (deduplicated, never the acting user). Failures are logged.
    pub async fn notify(
        &self,
        users: impl IntoIterator<Item = Id>,
        kind: NotificationKind,
        message: &str,
        target: Target,
    ) {
        let mut seen: Vec<Id> = Vec::new();
        for user_id in users {
            if seen.contains(&user_id) || Some(user_id) == target.actor_user {
                continue;
            }
            seen.push(user_id);
            let n = Notification {
                id: new_id(),
                user_id,
                kind,
                message: message.to_string(),
                ticket_id: target.ticket,
                comment_id: target.comment,
                run_id: target.run,
                job_id: target.job,
                actor_user_id: target.actor_user,
                actor_agent_id: target.actor_agent,
                read: false,
                created_at: Utc::now(),
            };
            if let Err(e) = self.db.insert_notification(&n).await {
                tracing::error!(error = ?e, "saving notification");
                continue;
            }
            self.hub.send_to_user(
                user_id,
                ServerMessage::Upsert {
                    item: Entity::Notification(n),
                },
            );
        }
    }
}
