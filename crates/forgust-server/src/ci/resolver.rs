//! Where `include:` files come from for a pipeline (forgust-ci-config's Resolver).

use async_trait::async_trait;
use forgust_ci_config::Resolver;
use globset::GlobBuilder;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, can_project};
use crate::state::AppState;

const MAX_FILE: usize = 1 << 20;

pub struct RepoResolver {
    pub state: AppState,
    pub project_id: i64,
    pub sha: String,
    /// The pipeline's user: project includes need their read access.
    pub user: Option<forgust_db::users::User>,
    /// What `include:rules` test.
    pub vars: std::collections::HashMap<String, String>,
}

impl RepoResolver {
    /// Every path of another project at a ref (`rules:exists:project`).
    pub async fn paths_of(&self, project: &str, git_ref: Option<&str>) -> Result<Vec<String>, String> {
        let (p, sha) = self.other(project, git_ref).await?;
        Ok(self.state.storage.list_paths(p.id, &sha, 100_000).await.map_err(|e| e.to_string())?.0)
    }

    /// Another project the pipeline's user may read, and a ref of it as a commit.
    async fn other(&self, project: &str, git_ref: Option<&str>) -> Result<(forgust_db::projects::Project, String), String> {
        let p = crate::api::load_project(&self.state, project).await.map_err(|_| "project not found".to_string())?;
        let principal = match &self.user {
            Some(u) => Principal::User { user: u.clone(), scopes: None },
            None => Principal::Anonymous,
        };
        // same answer for "missing" and "no access", like the rest of the API
        if !can_project(&self.state, &principal, ProjectAction::ReadCode, &p).await.unwrap_or(false) {
            return Err("project not found".into());
        }
        let rev = match git_ref {
            Some(r) => r.to_string(),
            None => p.default_branch.clone().ok_or("the project has no default branch")?,
        };
        let full = crate::services::edits::qualify(&self.state, p.id, &rev).await.map_err(|e| e.to_string())?;
        let sha =
            self.state.storage.resolve(p.id, &full).await.map_err(|e| e.to_string())?.ok_or_else(|| format!("ref `{rev}` not found"))?;
        Ok((p, sha))
    }
}

async fn read_file(state: &AppState, project_id: i64, rev: &str, path: &str) -> Result<String, String> {
    let blob = state.storage.blob(project_id, rev, path, MAX_FILE).await.map_err(|e| match e {
        forgust_storage::StorageError::PathNotFound(_) => "file not found".to_string(),
        other => other.to_string(),
    })?;
    if blob.truncated {
        return Err("larger than 1 MB".into());
    }
    String::from_utf8(blob.data).map_err(|_| "not UTF-8".into())
}

#[async_trait]
impl Resolver for RepoResolver {
    async fn local(&self, path: &str) -> Result<String, String> {
        read_file(&self.state, self.project_id, &self.sha, path).await
    }

    async fn local_glob(&self, pattern: &str) -> Result<Vec<String>, String> {
        let m = GlobBuilder::new(pattern).literal_separator(true).build().map_err(|e| e.to_string())?.compile_matcher();
        let (paths, _) = self.state.storage.list_paths(self.project_id, &self.sha, 100_000).await.map_err(|e| e.to_string())?;
        Ok(paths.into_iter().filter(|p| m.is_match(p)).collect())
    }

    async fn project(&self, project: &str, git_ref: Option<&str>, file: &str) -> Result<String, String> {
        let (p, sha) = self.other(project, git_ref).await?;
        read_file(&self.state, p.id, &sha, file).await
    }

    async fn remote(&self, url: &str) -> Result<String, String> {
        crate::services::webhooks::safe_get(&self.state, url, MAX_FILE).await
    }

    /// GitLab's component layout: `templates/<name>.yml` or
    /// `templates/<name>/template.yml`; `~latest` is the newest release.
    async fn component(&self, project: &str, name: &str, version: &str) -> Result<String, String> {
        if name.contains("..") || name.contains('/') {
            return Err("bad component name".into());
        }
        let version = if version == "~latest" {
            // access first: whether a private project exists or has releases is not told
            let (p, _) = self.other(project, None).await?;
            forgust_db::releases::latest_tag(&self.state.db, p.id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("the component's project has no release yet")?
        } else {
            version.to_string()
        };
        let (p, sha) = self.other(project, Some(&version)).await?;
        match read_file(&self.state, p.id, &sha, &format!("templates/{name}.yml")).await {
            Ok(t) => Ok(t),
            Err(_) => read_file(&self.state, p.id, &sha, &format!("templates/{name}/template.yml"))
                .await
                .map_err(|_| format!("no templates/{name}.yml or templates/{name}/template.yml at {version}")),
        }
    }

    fn server_host(&self) -> String {
        crate::registry::host(&self.state)
    }

    fn variables(&self) -> std::collections::HashMap<String, String> {
        self.vars.clone()
    }
}
