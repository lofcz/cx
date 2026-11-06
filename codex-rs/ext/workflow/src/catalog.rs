//! Saved workflow inventory. Resolution uses the same precedence as the Workflow tool.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Weak;

use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflow::validate_workflow_script;
use sha2::Digest;
use sha2::Sha256;

use crate::discovery::PluginWorkflowRoot;
use crate::discovery::WorkflowOrigin;
use crate::discovery::active_plugin_workflow_roots;
use crate::discovery::resolve_named;

pub const AUTHORING_GUIDE: &str =
    include_str!("../../../skills/src/assets/samples/workflow/references/api.md");

#[derive(Clone, Debug)]
pub struct WorkflowDefinition {
    pub name: String,
    pub description: String,
    pub origin: String,
    pub path: Option<String>,
    pub editable: bool,
    pub deletable: bool,
    pub error: Option<String>,
}

pub struct WorkflowDefinitionSource {
    pub definition: WorkflowDefinition,
    pub source: String,
    pub sha256: String,
    pub edit_path: String,
}

/// The catalog stays on the app-server host, including for remote TUI clients.
pub struct WorkflowCatalog {
    cwd: AbsolutePathBuf,
    home: AbsolutePathBuf,
    plugins: Vec<PluginWorkflowRoot>,
}

impl WorkflowCatalog {
    pub async fn new(config: &Config, manager: &Weak<ThreadManager>) -> Self {
        Self {
            cwd: config.cwd.clone(),
            home: config.codex_home.clone(),
            plugins: active_plugin_workflow_roots(manager, config).await,
        }
    }

    pub fn personal_directory(&self) -> String {
        self.home.join("workflows").display().to_string()
    }

    pub async fn list(&self) -> Result<Vec<WorkflowDefinition>, String> {
        let mut names: BTreeSet<String> = crate::bundled::names().map(str::to_string).collect();
        collect_names(&self.home.join("workflows"), None, &mut names).await?;
        for ancestor in self.cwd.ancestors() {
            collect_names(&ancestor.join(".codex/workflows"), None, &mut names).await?;
        }
        for plugin in &self.plugins {
            collect_names(&plugin.workflows_dir, Some(&plugin.namespace), &mut names).await?;
        }
        let mut definitions = Vec::new();
        for name in names {
            match self.read(&name).await {
                Ok(value) => definitions.push(value.definition),
                Err(error) => definitions.push(WorkflowDefinition {
                    name,
                    description: "Cannot read this workflow".to_string(),
                    origin: "unavailable".to_string(),
                    path: None,
                    editable: false,
                    deletable: false,
                    error: Some(error),
                }),
            }
        }
        Ok(definitions)
    }

    pub async fn read(&self, name: &str) -> Result<WorkflowDefinitionSource, String> {
        let (source, origin, _) = resolve_named(name, &self.cwd, &self.home, &self.plugins)
            .await
            .map_err(|error| error.to_string())?;
        let validation = validate_workflow_script(&source);
        let (origin_label, path, deletable) = match &origin {
            WorkflowOrigin::Bundled => ("built-in", None, false),
            WorkflowOrigin::Plugin { path, .. } => ("plugin", Some(path), false),
            WorkflowOrigin::File { path } => (
                if path.parent() == Some(self.home.join("workflows")) {
                    "personal"
                } else {
                    "project"
                },
                Some(path),
                true,
            ),
            WorkflowOrigin::Inline => {
                return Err("Inline workflows are not saved definitions".into());
            }
        };
        // Plugin names contain ':', which cannot be used in Windows file names.
        // Editing a plugin creates a separate personal workflow with a portable name.
        let edit_path = if deletable {
            path.map(|path| path.display().to_string())
                .unwrap_or_default()
        } else {
            self.home
                .join("workflows")
                .join(format!("{}.js", name.replace(':', "-")))
                .display()
                .to_string()
        };
        Ok(WorkflowDefinitionSource {
            definition: WorkflowDefinition {
                name: name.to_string(),
                description: validation
                    .as_ref()
                    .map(|script| script.meta.description.chars().take(512).collect())
                    .unwrap_or_else(|_| "Invalid workflow; edit it to repair it".to_string()),
                origin: origin_label.to_string(),
                path: path.map(|path| path.display().to_string()),
                editable: true,
                deletable,
                error: validation.err().map(|error| error.to_string()),
            },
            sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
            source,
            edit_path,
        })
    }

    /// Move the selected saved file out of discovery. Re-resolve to reject stale selections.
    /// The trash copy also preserves edits made concurrently with the final rename.
    pub async fn delete(
        &self,
        name: &str,
        expected_path: &str,
        sha256: &str,
    ) -> Result<(), String> {
        let current = self.read(name).await?;
        if !current.definition.deletable {
            return Err(
                "Built-in and plugin workflows cannot be deleted. Delete a personal copy instead."
                    .into(),
            );
        }
        if current.definition.path.as_deref() != Some(expected_path) || current.sha256 != sha256 {
            return Err("This workflow changed. Open it again before deleting it.".into());
        }
        let path = Path::new(expected_path);
        let parent = path.parent().ok_or("Workflow has no parent directory")?;
        let trash = parent.join(".trash");
        tokio::fs::create_dir_all(&trash)
            .await
            .map_err(|error| error.to_string())?;
        let metadata = tokio::fs::symlink_metadata(&trash)
            .await
            .map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Workflow trash must be a directory, not a symbolic link".into());
        }
        tokio::fs::rename(path, trash.join(format!("{}.js", uuid::Uuid::new_v4())))
            .await
            .map_err(|error| error.to_string())
    }
}

async fn collect_names(
    root: &Path,
    namespace: Option<&str>,
    names: &mut BTreeSet<String>,
) -> Result<(), String> {
    let mut directory = match tokio::fs::read_dir(root).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cannot list {}: {error}", root.display())),
    };
    let mut count = 0;
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        count += 1;
        if count > 4096 || names.len() > 1024 {
            return Err("Too many workflow files; keep at most 1024 saved workflows".into());
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("js") {
            continue;
        }
        if let Some(name) = path.file_stem().and_then(|value| value.to_str()) {
            names.insert(namespace.map_or_else(
                || name.to_string(),
                |namespace| format!("{namespace}:{name}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
