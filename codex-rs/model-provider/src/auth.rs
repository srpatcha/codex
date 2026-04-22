use std::sync::Arc;

use codex_agent_identity::AgentIdentityKey;
use codex_agent_identity::AgentRuntimeId;
use codex_agent_identity::AgentTaskAuthorizationTarget;
use codex_agent_identity::AgentTaskId;
use codex_agent_identity::AgentTaskKind;
use codex_agent_identity::RegisteredAgentTask;
use codex_agent_identity::authorization_header_for_agent_task;
use codex_agent_identity::authorization_header_for_registered_task;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::auth::AgentIdentityAuth;
use codex_model_provider_info::ModelProviderInfo;
use http::HeaderMap;
use http::HeaderValue;

use crate::bearer_auth_provider::BearerAuthProvider;

#[derive(Clone, Debug)]
pub struct AgentTaskAuth {
    auth: AgentIdentityAuth,
    task: RegisteredAgentTask,
}

impl AgentTaskAuth {
    pub fn new(auth: AgentIdentityAuth, task: RegisteredAgentTask) -> Self {
        Self { auth, task }
    }
}

#[derive(Clone, Debug)]
struct AgentIdentityAuthProvider {
    auth: AgentIdentityAuth,
    task: Option<RegisteredAgentTask>,
}

impl AuthProvider for AgentIdentityAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let record = self.auth.record();
        let header_value = match self.task.as_ref() {
            Some(task) => authorization_header_for_registered_task(
                AgentIdentityKey {
                    agent_runtime_id: &record.agent_runtime_id,
                    private_key_pkcs8_base64: &record.agent_private_key,
                },
                task,
            )
            .map_err(std::io::Error::other),
            None => self
                .auth
                .process_task_id()
                .ok_or_else(|| {
                    std::io::Error::other("agent identity process task is not initialized")
                })
                .and_then(|task_id| {
                    authorization_header_for_agent_task(
                        AgentIdentityKey {
                            agent_runtime_id: &record.agent_runtime_id,
                            private_key_pkcs8_base64: &record.agent_private_key,
                        },
                        AgentTaskAuthorizationTarget {
                            agent_runtime_id: &record.agent_runtime_id,
                            task_id,
                        },
                    )
                    .map_err(std::io::Error::other)
                }),
        };

        if let Ok(header_value) = header_value
            && let Ok(header) = HeaderValue::from_str(&header_value)
        {
            let _ = headers.insert(http::header::AUTHORIZATION, header);
        }

        if let Ok(header) = HeaderValue::from_str(self.auth.account_id()) {
            let _ = headers.insert("ChatGPT-Account-ID", header);
        }

        if self.auth.is_fedramp_account() {
            let _ = headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
        }
    }
}

// Some providers are meant to send no auth headers. Examples include local OSS
// providers and custom test providers with `requires_openai_auth = false`.
#[derive(Clone, Debug)]
struct UnauthenticatedAuthProvider;

impl AuthProvider for UnauthenticatedAuthProvider {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
}

pub fn unauthenticated_auth_provider() -> SharedAuthProvider {
    Arc::new(UnauthenticatedAuthProvider)
}

/// Returns the provider-scoped auth manager when this provider uses command-backed auth.
///
/// Providers without custom auth continue using the caller-supplied base manager, when present.
pub(crate) fn auth_manager_for_provider(
    auth_manager: Option<Arc<AuthManager>>,
    provider: &ModelProviderInfo,
) -> Option<Arc<AuthManager>> {
    match provider.auth.clone() {
        Some(config) => Some(AuthManager::external_bearer_only(config)),
        None => auth_manager,
    }
}

pub(crate) fn resolve_provider_auth(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
    agent_task_auth: Option<AgentTaskAuth>,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if let Some(auth) = bearer_auth_for_provider(provider)? {
        return Ok(Arc::new(auth));
    }

    if let Some(agent_task_auth) = agent_task_auth
        && provider_uses_first_party_auth_path(provider)
    {
        return Ok(auth_provider_from_agent_task(
            agent_task_auth.auth,
            agent_task_auth.task,
        ));
    }

    Ok(match auth {
        Some(auth) => auth_provider_from_auth(auth),
        None => unauthenticated_auth_provider(),
    })
}

fn bearer_auth_for_provider(
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<BearerAuthProvider>> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(Some(BearerAuthProvider::new(api_key)));
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(BearerAuthProvider::new(token)));
    }

    Ok(None)
}

pub fn provider_uses_first_party_auth_path(provider: &ModelProviderInfo) -> bool {
    provider.requires_openai_auth
        && provider.env_key.is_none()
        && provider.experimental_bearer_token.is_none()
        && provider.auth.is_none()
        && provider.aws.is_none()
}

/// Builds request-header auth for a first-party Codex auth snapshot.
pub fn auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    match auth {
        CodexAuth::AgentIdentity(auth) => Arc::new(AgentIdentityAuthProvider {
            auth: auth.clone(),
            task: None,
        }),
        CodexAuth::ApiKey(_) | CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => {
            Arc::new(BearerAuthProvider {
                token: auth.get_token().ok(),
                account_id: auth.get_account_id(),
                is_fedramp_account: auth.is_fedramp_account(),
            })
        }
    }
}

pub fn auth_provider_from_agent_task(
    auth: AgentIdentityAuth,
    task: RegisteredAgentTask,
) -> SharedAuthProvider {
    Arc::new(AgentIdentityAuthProvider {
        auth,
        task: Some(task),
    })
}

/// Builds background/control-plane auth from the concrete auth snapshot.
///
/// ChatGPT callers that have opted into Agent Identity should first resolve the
/// effective [`AgentIdentityAuth`] and call
/// [`background_auth_provider_from_agent_identity_auth`].
pub async fn background_auth_provider_from_auth(
    auth: &CodexAuth,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<SharedAuthProvider> {
    match auth {
        CodexAuth::AgentIdentity(auth) => {
            background_auth_provider_from_agent_identity_auth(auth.clone(), chatgpt_base_url).await
        }
        CodexAuth::ApiKey(_) | CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => {
            Ok(auth_provider_from_auth(auth))
        }
    }
}

pub async fn background_auth_provider_from_agent_identity_auth(
    auth: AgentIdentityAuth,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<SharedAuthProvider> {
    auth.ensure_background_task(chatgpt_base_url).await?;
    let task_id = auth.background_task_id().ok_or_else(|| {
        std::io::Error::other("agent identity background task is not initialized")
    })?;
    Ok(auth_provider_from_agent_task(
        auth.clone(),
        RegisteredAgentTask::new(
            AgentRuntimeId::new(auth.record().agent_runtime_id.clone()),
            AgentTaskId::new(task_id.to_string()),
            AgentTaskKind::Background,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use codex_agent_identity::generate_agent_key_material;
    use codex_login::auth::AgentIdentityAuthRecord;
    use codex_model_provider_info::WireApi;
    use codex_model_provider_info::create_oss_provider_with_base_url;
    use codex_protocol::account::PlanType;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn unauthenticated_auth_provider_adds_no_headers() {
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        let auth =
            resolve_provider_auth(/*auth*/ None, &provider, /*agent_task_auth*/ None)
                .expect("auth should resolve");

        assert!(auth.to_auth_headers().is_empty());
    }

    #[test]
    fn agent_task_auth_provider_preserves_account_routing_headers() {
        let key_material = generate_agent_key_material().expect("generate key material");
        let auth = codex_login::auth::AgentIdentityAuth::new(AgentIdentityAuthRecord {
            agent_runtime_id: "agent-runtime-1".to_string(),
            agent_private_key: key_material.private_key_pkcs8_base64,
            account_id: "account-1".to_string(),
            chatgpt_user_id: "user-1".to_string(),
            email: "agent@example.com".to_string(),
            plan_type: PlanType::Plus,
            chatgpt_account_is_fedramp: true,
            registered_at: None,
        });
        let provider = auth_provider_from_agent_task(
            auth,
            RegisteredAgentTask::new(
                AgentRuntimeId::new("agent-runtime-1"),
                AgentTaskId::new("background-task-1"),
                AgentTaskKind::Background,
            ),
        );

        let headers = provider.to_auth_headers();

        assert!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("AgentAssertion "))
        );
        assert_eq!(
            headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            Some("account-1")
        );
        assert_eq!(
            headers
                .get("X-OpenAI-Fedramp")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
    }

    #[test]
    fn provider_auth_ignores_agent_task_for_non_openai_provider() {
        let key_material = generate_agent_key_material().expect("generate key material");
        let auth = AgentIdentityAuth::new(AgentIdentityAuthRecord {
            agent_runtime_id: "agent-runtime-1".to_string(),
            agent_private_key: key_material.private_key_pkcs8_base64,
            account_id: "account-1".to_string(),
            chatgpt_user_id: "user-1".to_string(),
            email: "agent@example.com".to_string(),
            plan_type: PlanType::Plus,
            chatgpt_account_is_fedramp: false,
            registered_at: None,
        });
        let task_auth = AgentTaskAuth::new(
            auth,
            RegisteredAgentTask::new(
                AgentRuntimeId::new("agent-runtime-1"),
                AgentTaskId::new("thread-task-1"),
                AgentTaskKind::Thread,
            ),
        );
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);

        let auth = resolve_provider_auth(
            /*auth*/ None,
            &provider,
            /*agent_task_auth*/ Some(task_auth),
        )
        .expect("auth should resolve");

        assert!(auth.to_auth_headers().is_empty());
    }

    #[test]
    fn first_party_auth_path_excludes_provider_specific_auth() {
        let mut env_key_provider =
            ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        env_key_provider.env_key = Some("OPENAI_API_KEY".to_string());
        assert!(!provider_uses_first_party_auth_path(&env_key_provider));

        let bedrock_provider = ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None);
        assert!(!provider_uses_first_party_auth_path(&bedrock_provider));
    }
}
