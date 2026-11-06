use super::*;

fn catalog(root: &Path) -> WorkflowCatalog {
    WorkflowCatalog {
        cwd: AbsolutePathBuf::try_from(root.join("project/nested")).unwrap(),
        home: AbsolutePathBuf::try_from(root.join("home")).unwrap(),
        plugins: Vec::new(),
    }
}

async fn save(path: &Path, name: &str, description: &str) {
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(
        path,
        format!(
            "export const meta = {{name: {name:?}, description: {description:?}}};\nreturn args;"
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn catalog_uses_run_precedence_and_delete_reveals_inherited_definition() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    let personal = root.path().join("home/workflows/code-review.js");
    let project = root.path().join("project/.codex/workflows/code-review.js");
    save(&personal, "code-review", "Personal").await;
    save(&project, "code-review", "Project").await;
    let selected = catalog.read("code-review").await.unwrap();
    assert_eq!(selected.definition.description, "Project");
    assert_eq!(selected.definition.origin, "project");
    catalog
        .delete(
            "code-review",
            selected.definition.path.as_deref().unwrap(),
            &selected.sha256,
        )
        .await
        .unwrap();
    assert!(!project.exists());
    assert!(project.parent().unwrap().join(".trash").exists());
    assert_eq!(
        catalog
            .read("code-review")
            .await
            .unwrap()
            .definition
            .description,
        "Personal"
    );
    let selected = catalog.read("code-review").await.unwrap();
    catalog
        .delete(
            "code-review",
            selected.definition.path.as_deref().unwrap(),
            &selected.sha256,
        )
        .await
        .unwrap();
    let builtin = catalog.read("code-review").await.unwrap();
    assert_eq!(builtin.definition.origin, "built-in");
    assert!(!builtin.definition.deletable);
    assert_eq!(builtin.edit_path, personal.display().to_string());
}

#[tokio::test]
async fn catalog_rejects_stale_delete_and_path_substitution() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    let path = root.path().join("home/workflows/check.js");
    save(&path, "check", "First").await;
    let first = catalog.read("check").await.unwrap();
    save(&path, "check", "Updated").await;
    assert!(
        catalog
            .delete("check", &path.display().to_string(), &first.sha256)
            .await
            .is_err()
    );
    let updated = catalog.read("check").await.unwrap();
    assert!(
        catalog
            .delete("check", "/some/other/file.js", &updated.sha256)
            .await
            .is_err()
    );
    assert_eq!(
        catalog.read("check").await.unwrap().definition.description,
        "Updated"
    );
    assert!(catalog.read("../check").await.is_err());
    assert!(
        catalog
            .delete("code-review", &path.display().to_string(), &updated.sha256)
            .await
            .is_err()
    );
    assert!(path.exists());
}

#[tokio::test]
async fn catalog_keeps_invalid_scripts_editable_and_lists_builtins() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    let path = root.path().join("home/workflows/broken.js");
    save(&path, "broken", "initial").await;
    tokio::fs::write(&path, "invalid script").await.unwrap();
    let entries = catalog.list().await.unwrap();
    assert_eq!(entries.len(), 3);
    let broken = entries.iter().find(|entry| entry.name == "broken").unwrap();
    assert!(broken.error.is_some());
    assert!(broken.editable && broken.deletable);
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == "deep-research" && entry.error.is_none())
    );
}

#[tokio::test]
async fn catalog_includes_active_plugin_roots_without_changing_plugin_files() {
    let root = tempfile::tempdir().unwrap();
    let mut catalog = catalog(root.path());
    let path = root.path().join("plugin/workflows/check.js");
    save(&path, "check", "Plugin check").await;
    catalog.plugins.push(PluginWorkflowRoot {
        namespace: "tools".to_string(),
        workflows_dir: AbsolutePathBuf::try_from(path.parent().unwrap().to_path_buf()).unwrap(),
    });
    let entries = catalog.list().await.unwrap();
    assert!(entries.iter().any(|entry| entry.name == "tools:check"));
    let selected = catalog.read("tools:check").await.unwrap();
    assert!(!selected.definition.deletable);
    assert!(selected.edit_path.ends_with("tools-check.js"));
    assert!(
        catalog
            .delete("tools:check", &path.display().to_string(), &selected.sha256)
            .await
            .is_err()
    );
    assert!(path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn catalog_delete_does_not_follow_a_trash_symlink() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    let path = root.path().join("home/workflows/check.js");
    save(&path, "check", "Check").await;
    let outside = root.path().join("outside");
    tokio::fs::create_dir(&outside).await.unwrap();
    std::os::unix::fs::symlink(&outside, path.parent().unwrap().join(".trash")).unwrap();
    let selected = catalog.read("check").await.unwrap();
    assert!(
        catalog
            .delete("check", &path.display().to_string(), &selected.sha256)
            .await
            .is_err()
    );
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}
