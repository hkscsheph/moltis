use std::sync::Arc;

use {async_trait::async_trait, serde_json::Value, tokio::sync::RwLock, tracing::warn};

use moltis_projects::{
    ProjectStore, TomlProjectStore, complete::complete_path, context::load_context_files,
    detect::auto_detect,
};

use crate::services::{ProjectService, ServiceResult};

pub struct LiveProjectService {
    store: Arc<RwLock<TomlProjectStore>>,
}

impl LiveProjectService {
    pub fn new(store: TomlProjectStore) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
        }
    }
}

#[async_trait]
impl ProjectService for LiveProjectService {
    async fn list(&self) -> ServiceResult {
        let store = self.store.read().await;
        let projects = store.list().await.map_err(|e| e.to_string())?;
        serde_json::to_value(projects).map_err(|e| e.to_string())
    }

    async fn get(&self, params: Value) -> ServiceResult {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'id' parameter".to_string())?;
        let store = self.store.read().await;
        let project = store.get(id).await.map_err(|e| e.to_string())?;
        serde_json::to_value(project).map_err(|e| e.to_string())
    }

    async fn upsert(&self, params: Value) -> ServiceResult {
        let project: moltis_projects::Project =
            serde_json::from_value(params).map_err(|e| e.to_string())?;
        let store = self.store.read().await;
        store
            .upsert(project.clone())
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(project).map_err(|e| e.to_string())
    }

    async fn delete(&self, params: Value) -> ServiceResult {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'id' parameter".to_string())?;
        let store = self.store.read().await;
        store.delete(id).await.map_err(|e| e.to_string())?;
        Ok(serde_json::json!({"deleted": id}))
    }

    async fn detect(&self, params: Value) -> ServiceResult {
        let dirs: Vec<String> = params
            .get("directories")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let store = self.store.read().await;
        let existing = store.list().await.map_err(|e| e.to_string())?;
        let known_ids: Vec<String> = existing.iter().map(|p| p.id.clone()).collect();

        let dir_refs: Vec<std::path::PathBuf> = dirs.iter().map(std::path::PathBuf::from).collect();
        let dir_slices: Vec<&std::path::Path> = dir_refs.iter().map(|p| p.as_path()).collect();
        let detected = auto_detect(&dir_slices, &known_ids);

        // Persist detected projects
        for p in &detected {
            if let Err(e) = store.upsert(p.clone()).await {
                warn!(id = %p.id, error = %e, "failed to persist detected project");
            }
        }

        serde_json::to_value(detected).map_err(|e| e.to_string())
    }

    async fn complete_path(&self, params: Value) -> ServiceResult {
        let partial = params.get("partial").and_then(|v| v.as_str()).unwrap_or("");
        let results: Vec<String> = complete_path(partial)
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        serde_json::to_value(results).map_err(|e| e.to_string())
    }

    async fn context(&self, params: Value) -> ServiceResult {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'id' parameter".to_string())?;
        let store = self.store.read().await;
        let project = store
            .get(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("project '{id}' not found"))?;

        let context_files = load_context_files(&project.directory).map_err(|e| e.to_string())?;
        let entries: Vec<Value> = context_files
            .iter()
            .map(|cf| {
                serde_json::json!({
                    "path": cf.path.to_string_lossy(),
                    "content": cf.content,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "project": project,
            "context_files": entries,
        }))
    }
}
