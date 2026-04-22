use codex_features::Feature;
use codex_login::auth::AgentIdentityAuth;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionAgentTask;
use tracing::debug;

use crate::session::session::Session;

impl Session {
    fn latest_persisted_agent_task(
        rollout_items: &[RolloutItem],
    ) -> Option<Option<SessionAgentTask>> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::SessionState(update) => Some(update.agent_task.clone()),
            _ => None,
        })
    }

    fn persisted_agent_task_for_runtime(
        rollout_items: &[RolloutItem],
        agent_runtime_id: Option<&str>,
    ) -> Option<Option<SessionAgentTask>> {
        let latest = Self::latest_persisted_agent_task(rollout_items)?;
        match latest {
            Some(agent_task)
                if agent_runtime_id.is_some_and(|agent_runtime_id| {
                    agent_task.agent_runtime_id == agent_runtime_id
                }) =>
            {
                Some(Some(agent_task))
            }
            Some(agent_task) => {
                debug!(
                    agent_runtime_id = %agent_task.agent_runtime_id,
                    task_id = %agent_task.task_id,
                    "discarding persisted agent task because it does not match the current agent identity"
                );
                Some(None)
            }
            None => Some(None),
        }
    }

    pub(super) async fn restore_persisted_agent_task(&self, rollout_items: &[RolloutItem]) {
        let agent_identity = match self.current_agent_identity_auth().await {
            Ok(Some(agent_identity)) => agent_identity,
            Ok(None) => return,
            Err(err) => {
                debug!("skipping persisted agent task restore: {err:#}");
                return;
            }
        };
        let agent_runtime_id = Some(agent_identity.record().agent_runtime_id.as_str());
        let Some(agent_task_update) =
            Self::persisted_agent_task_for_runtime(rollout_items, agent_runtime_id)
        else {
            return;
        };

        match agent_task_update {
            Some(agent_task) => {
                let mut state = self.state.lock().await;
                state.set_agent_task(agent_task);
            }
            None => {
                let mut state = self.state.lock().await;
                state.clear_agent_task();
            }
        }
    }

    pub(crate) async fn current_agent_identity_auth(
        &self,
    ) -> std::io::Result<Option<AgentIdentityAuth>> {
        let policy = if self.features.enabled(Feature::UseAgentIdentity) {
            AgentIdentityAuthPolicy::JwtOrChatgpt
        } else {
            AgentIdentityAuthPolicy::JwtOnly
        };
        let session_source = {
            let state = self.state.lock().await;
            state.session_configuration.session_source.clone()
        };
        self.services
            .auth_manager
            .agent_identity_auth(policy, session_source)
            .await
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::protocol::SessionAgentTaskKind;
    use codex_protocol::protocol::SessionStateUpdate;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn latest_persisted_agent_task_uses_latest_update() {
        let first = SessionAgentTask {
            agent_runtime_id: "agent-1".to_string(),
            task_id: "task-1".to_string(),
            kind: SessionAgentTaskKind::Thread,
        };
        let second = SessionAgentTask {
            agent_runtime_id: "agent-1".to_string(),
            task_id: "task-2".to_string(),
            kind: SessionAgentTaskKind::Thread,
        };

        let latest = Session::latest_persisted_agent_task(&[
            RolloutItem::SessionState(SessionStateUpdate {
                agent_task: Some(first),
            }),
            RolloutItem::SessionState(SessionStateUpdate {
                agent_task: Some(second.clone()),
            }),
        ]);

        assert_eq!(latest, Some(Some(second)));
    }

    #[test]
    fn latest_persisted_agent_task_preserves_explicit_clear() {
        let task = SessionAgentTask {
            agent_runtime_id: "agent-1".to_string(),
            task_id: "task-1".to_string(),
            kind: SessionAgentTaskKind::Thread,
        };

        let latest = Session::latest_persisted_agent_task(&[
            RolloutItem::SessionState(SessionStateUpdate {
                agent_task: Some(task),
            }),
            RolloutItem::SessionState(SessionStateUpdate { agent_task: None }),
        ]);

        assert_eq!(latest, Some(None));
    }

    #[test]
    fn persisted_agent_task_for_runtime_restores_matching_task() {
        let task = SessionAgentTask {
            agent_runtime_id: "agent-1".to_string(),
            task_id: "task-1".to_string(),
            kind: SessionAgentTaskKind::Thread,
        };

        let restored = Session::persisted_agent_task_for_runtime(
            &[RolloutItem::SessionState(SessionStateUpdate {
                agent_task: Some(task.clone()),
            })],
            Some("agent-1"),
        );

        assert_eq!(restored, Some(Some(task)));
    }

    #[test]
    fn persisted_agent_task_for_runtime_clears_mismatched_task() {
        let task = SessionAgentTask {
            agent_runtime_id: "agent-1".to_string(),
            task_id: "task-1".to_string(),
            kind: SessionAgentTaskKind::Thread,
        };

        let restored = Session::persisted_agent_task_for_runtime(
            &[RolloutItem::SessionState(SessionStateUpdate {
                agent_task: Some(task),
            })],
            Some("agent-2"),
        );

        assert_eq!(restored, Some(None));
    }
}
