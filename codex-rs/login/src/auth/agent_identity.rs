use std::sync::Arc;

use codex_agent_identity::AgentIdentityKey;
use codex_agent_identity::normalize_chatgpt_base_url;
use codex_agent_identity::register_agent_task;
use codex_protocol::account::PlanType as AccountPlanType;
use tokio::sync::OnceCell;

use crate::default_client::build_reqwest_client;

use super::storage::AgentIdentityAuthRecord;

const DEFAULT_CHATGPT_BACKEND_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug)]
pub struct AgentIdentityAuth {
    record: AgentIdentityAuthRecord,
    process_task_id: Arc<OnceCell<String>>,
    background_task_id: Arc<OnceCell<String>>,
}

impl Clone for AgentIdentityAuth {
    fn clone(&self) -> Self {
        Self {
            record: self.record.clone(),
            process_task_id: Arc::clone(&self.process_task_id),
            background_task_id: Arc::clone(&self.background_task_id),
        }
    }
}

impl AgentIdentityAuth {
    pub fn new(record: AgentIdentityAuthRecord) -> Self {
        Self {
            record,
            process_task_id: Arc::new(OnceCell::new()),
            background_task_id: Arc::new(OnceCell::new()),
        }
    }

    pub fn record(&self) -> &AgentIdentityAuthRecord {
        &self.record
    }

    pub fn process_task_id(&self) -> Option<&str> {
        self.process_task_id.get().map(String::as_str)
    }

    pub fn background_task_id(&self) -> Option<&str> {
        self.background_task_id.get().map(String::as_str)
    }

    pub async fn ensure_runtime(&self, chatgpt_base_url: Option<String>) -> std::io::Result<()> {
        self.process_task_id
            .get_or_try_init(|| async { self.register_task(chatgpt_base_url).await })
            .await
            .map(|_| ())
    }

    pub async fn ensure_background_task(
        &self,
        chatgpt_base_url: Option<String>,
    ) -> std::io::Result<()> {
        self.background_task_id
            .get_or_try_init(|| async { self.register_task(chatgpt_base_url).await })
            .await
            .map(|_| ())
    }

    pub async fn register_task(&self, chatgpt_base_url: Option<String>) -> std::io::Result<String> {
        let base_url = normalize_chatgpt_base_url(
            chatgpt_base_url
                .as_deref()
                .unwrap_or(DEFAULT_CHATGPT_BACKEND_BASE_URL),
        );
        register_agent_task(&build_reqwest_client(), &base_url, self.key())
            .await
            .map_err(std::io::Error::other)
    }

    pub fn account_id(&self) -> &str {
        &self.record.account_id
    }

    pub fn chatgpt_user_id(&self) -> &str {
        &self.record.chatgpt_user_id
    }

    pub fn email(&self) -> &str {
        &self.record.email
    }

    pub fn plan_type(&self) -> AccountPlanType {
        self.record.plan_type
    }

    pub fn is_fedramp_account(&self) -> bool {
        self.record.chatgpt_account_is_fedramp
    }

    fn key(&self) -> AgentIdentityKey<'_> {
        AgentIdentityKey {
            agent_runtime_id: &self.record.agent_runtime_id,
            private_key_pkcs8_base64: &self.record.agent_private_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn agent_identity_record() -> AgentIdentityAuthRecord {
        AgentIdentityAuthRecord {
            agent_runtime_id: "agent-runtime-1".to_string(),
            agent_private_key: "private-key".to_string(),
            account_id: "account-1".to_string(),
            chatgpt_user_id: "user-1".to_string(),
            email: "agent@example.com".to_string(),
            plan_type: AccountPlanType::Plus,
            chatgpt_account_is_fedramp: false,
            registered_at: None,
        }
    }

    #[test]
    fn background_task_slot_is_shared_across_clones() {
        let auth = AgentIdentityAuth::new(agent_identity_record());
        let cloned = auth.clone();

        auth.background_task_id
            .set("background-task-1".to_string())
            .expect("background task should be unset");

        assert_eq!(cloned.background_task_id(), Some("background-task-1"));
    }
}
