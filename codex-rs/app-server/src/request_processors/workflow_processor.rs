use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowAgentControlParams;
use codex_app_server_protocol::WorkflowAgentRetryResponse;
use codex_app_server_protocol::WorkflowAgentSkipResponse;
use codex_app_server_protocol::WorkflowApprovalArtifactReadParams;
use codex_app_server_protocol::WorkflowApprovalArtifactReadResponse;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowStopParams;
use codex_app_server_protocol::WorkflowStopResponse;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflow_extension::WorkflowService;
use codex_workflow_extension::WorkflowServiceError;
use codex_workflow_extension::read_workflow_approval_artifact;
use serde::Deserialize;
use serde::Serialize;

use crate::error_code::invalid_request;
use codex_app_server_protocol::WorkflowDefinition;
use codex_app_server_protocol::WorkflowDefinitionDeleteParams;
use codex_app_server_protocol::WorkflowDefinitionDeleteResponse;
use codex_app_server_protocol::WorkflowDefinitionListParams;
use codex_app_server_protocol::WorkflowDefinitionListResponse;
use codex_app_server_protocol::WorkflowDefinitionReadParams;
use codex_app_server_protocol::WorkflowDefinitionReadResponse;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_features::Feature;
use codex_workflow_extension::catalog::AUTHORING_GUIDE;
use codex_workflow_extension::catalog::WorkflowCatalog;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct WorkflowRequestProcessor {
    service: WorkflowService,
    codex_home: AbsolutePathBuf,
    config: Arc<Config>,
    thread_manager: Arc<ThreadManager>,
}

impl WorkflowRequestProcessor {
    pub(crate) fn new(
        service: WorkflowService,
        config: Arc<Config>,
        thread_manager: Arc<ThreadManager>,
    ) -> Self {
        Self {
            service,
            codex_home: config.codex_home.clone(),
            config,
            thread_manager,
        }
    }

    async fn catalog(&self, thread_id: Option<&str>) -> Result<WorkflowCatalog, JSONRPCErrorError> {
        let config = if let Some(id) = thread_id {
            let thread = self
                .thread_manager
                .get_thread(parse_thread_id(id)?)
                .await
                .map_err(|error| invalid_request(error.to_string()))?;
            if thread.session_source().is_non_root_agent() {
                return Err(invalid_request(
                    "Workflow management belongs to the owning thread",
                ));
            }
            let snapshot = thread.config_snapshot().await;
            let environments = self.thread_manager.environment_manager();
            for selection in snapshot.environment_selections() {
                let environment = environments
                    .get_environment(&selection.environment_id)
                    .ok_or_else(|| {
                        invalid_request("The selected workflow environment is unavailable")
                    })?;
                if environment.is_remote() {
                    return Err(invalid_request(
                        "The workflow library requires an execution environment on the app-server host. Remote workflows still support inline scripts.",
                    ));
                }
            }
            // The loaded config supplies plugin settings, but its original cwd can be stale
            // after /cd. Match the Workflow tool's primary execution directory.
            let mut config = (*thread.config().await).clone();
            config.cwd = match snapshot.environment_selections().first() {
                Some(primary) => primary
                    .cwd
                    .to_abs_path()
                    .map_err(|error| invalid_request(error.to_string()))?,
                None => snapshot.cwd().clone(),
            };
            Arc::new(config)
        } else {
            Arc::clone(&self.config)
        };
        if !config.features.enabled(Feature::Workflows) {
            return Err(invalid_request("Workflows are disabled"));
        }
        Ok(WorkflowCatalog::new(&config, &Arc::downgrade(&self.thread_manager)).await)
    }

    pub(crate) async fn definition_list(
        &self,
        params: WorkflowDefinitionListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let catalog = self.catalog(params.thread_id.as_deref()).await?;
        let data = catalog
            .list()
            .await
            .map_err(invalid_request)?
            .into_iter()
            .map(definition)
            .collect();
        Ok(Some(
            WorkflowDefinitionListResponse {
                data,
                personal_directory: catalog.personal_directory(),
                authoring_guide: AUTHORING_GUIDE.to_string(),
            }
            .into(),
        ))
    }

    pub(crate) async fn definition_read(
        &self,
        params: WorkflowDefinitionReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let catalog = self.catalog(params.thread_id.as_deref()).await?;
        let value = catalog.read(&params.name).await.map_err(invalid_request)?;
        Ok(Some(
            WorkflowDefinitionReadResponse {
                definition: definition(value.definition),
                source: value.source,
                sha256: value.sha256,
                edit_path: value.edit_path,
                authoring_guide: AUTHORING_GUIDE.to_string(),
            }
            .into(),
        ))
    }

    pub(crate) async fn definition_delete(
        &self,
        params: WorkflowDefinitionDeleteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let catalog = self.catalog(params.thread_id.as_deref()).await?;
        catalog
            .delete(&params.name, &params.expected_path, &params.sha256)
            .await
            .map_err(invalid_request)?;
        Ok(Some(
            WorkflowDefinitionDeleteResponse { deleted: true }.into(),
        ))
    }

    pub(crate) async fn read_approval_artifact(
        &self,
        params: WorkflowApprovalArtifactReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let artifact = read_workflow_approval_artifact(
            &self.codex_home,
            thread_id,
            &params.artifact_id,
            params.offset.unwrap_or(0),
        )
        .await
        .map_err(invalid_request)?;
        Ok(Some(
            WorkflowApprovalArtifactReadResponse {
                sha256: artifact.sha256,
                offset: artifact.offset,
                contents: artifact.contents,
                next_offset: artifact.next_offset,
            }
            .into(),
        ))
    }

    pub(crate) async fn list(
        &self,
        params: WorkflowListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let tasks = self.service.list(thread_id).await.map_err(service_error)?;
        let cursor = params
            .cursor
            .as_deref()
            .map(serde_json::from_str::<WorkflowListCursor>)
            .transpose()
            .map_err(|_| invalid_request("invalid workflow list cursor"))?;
        let limit = usize::try_from(params.limit.unwrap_or(50).clamp(1, 100)).unwrap_or(100);
        let matching = tasks
            .iter()
            .filter(|snapshot| {
                cursor.as_ref().is_none_or(|cursor| {
                    snapshot.started_at < cursor.started_at
                        || (snapshot.started_at == cursor.started_at
                            && snapshot.run_id < cursor.run_id)
                })
            })
            .collect::<Vec<_>>();
        let page = matching.iter().take(limit).copied().collect::<Vec<_>>();
        let data = page
            .iter()
            .copied()
            .cloned()
            .map(crate::workflow_events::task)
            .collect::<Vec<_>>();
        let next_cursor = (page.len() < matching.len())
            .then(|| page.last().copied())
            .flatten()
            .map(|snapshot| {
                serde_json::to_string(&WorkflowListCursor {
                    started_at: snapshot.started_at,
                    run_id: snapshot.run_id.clone(),
                })
                .map_err(|error| invalid_request(error.to_string()))
            })
            .transpose()?;
        Ok(Some(WorkflowListResponse { data, next_cursor }.into()))
    }

    pub(crate) async fn stop(
        &self,
        params: WorkflowStopParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let accepted = self
            .service
            .stop(thread_id, &params.run_id)
            .await
            .map_err(service_error)?;
        Ok(Some(WorkflowStopResponse { accepted }.into()))
    }

    pub(crate) async fn skip_agent(
        &self,
        params: WorkflowAgentControlParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let accepted = self
            .service
            .skip_agent(thread_id, &params.run_id, params.agent_index)
            .await
            .map_err(service_error)?;
        Ok(Some(WorkflowAgentSkipResponse { accepted }.into()))
    }

    pub(crate) async fn retry_agent(
        &self,
        params: WorkflowAgentControlParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let accepted = self
            .service
            .retry_agent(thread_id, &params.run_id, params.agent_index)
            .await
            .map_err(service_error)?;
        Ok(Some(WorkflowAgentRetryResponse { accepted }.into()))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkflowListCursor {
    started_at: i64,
    run_id: String,
}

fn parse_thread_id(value: &str) -> Result<ThreadId, JSONRPCErrorError> {
    ThreadId::from_string(value).map_err(|error| invalid_request(error.to_string()))
}

fn service_error(error: WorkflowServiceError) -> JSONRPCErrorError {
    match error {
        WorkflowServiceError::NotFound
        | WorkflowServiceError::WrongThread
        | WorkflowServiceError::StillRunning
        | WorkflowServiceError::Persistence(_) => invalid_request(error.to_string()),
    }
}

fn definition(value: codex_workflow_extension::catalog::WorkflowDefinition) -> WorkflowDefinition {
    WorkflowDefinition {
        name: value.name,
        description: value.description,
        origin: value.origin,
        path: value.path,
        editable: value.editable,
        deletable: value.deletable,
        error: value.error,
    }
}
