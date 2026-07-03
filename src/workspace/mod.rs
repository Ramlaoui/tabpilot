use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lsp_types::{InitializeParams, Url};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceContext {
    pub root_uri: Option<String>,
    pub name: Option<String>,
    pub git: Option<GitContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitContext {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub remotes: Vec<GitRemote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

struct WorkspaceRoot {
    uri: String,
    name: Option<String>,
    path: Option<PathBuf>,
}

impl WorkspaceContext {
    pub fn from_initialize_params(params: &InitializeParams) -> Self {
        let Some(root) = workspace_root(params) else {
            return Self::default();
        };

        let git = root.path.as_deref().and_then(git_context);

        Self {
            root_uri: Some(root.uri),
            name: root.name,
            git,
        }
    }
}

fn workspace_root(params: &InitializeParams) -> Option<WorkspaceRoot> {
    if let Some(folder) = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
    {
        return Some(root_from_url(&folder.uri, Some(folder.name.clone())));
    }

    #[allow(deprecated)]
    params.root_uri.as_ref().map(|uri| root_from_url(uri, None))
}

fn root_from_url(url: &Url, explicit_name: Option<String>) -> WorkspaceRoot {
    let path = url.to_file_path().ok();
    let path_name = path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let name = explicit_name
        .filter(|name| !name.contains('/') && !name.contains('\\'))
        .or(path_name);

    WorkspaceRoot {
        uri: url.to_string(),
        name,
        path,
    }
}

fn git_context(root: &Path) -> Option<GitContext> {
    let branch = git_output(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let head = git_output(root, &["rev-parse", "HEAD"]);
    let remotes = git_remotes(root);

    if branch.is_none() && head.is_none() && remotes.is_empty() {
        return None;
    }

    Some(GitContext {
        branch,
        head,
        remotes,
    })
}

fn git_remotes(root: &Path) -> Vec<GitRemote> {
    let Some(output) = git_output(root, &["remote", "-v"]) else {
        return Vec::new();
    };

    let mut remotes = BTreeMap::new();

    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else {
            continue;
        };
        let Some(url) = parts.next() else {
            continue;
        };
        let Some(kind) = parts.next() else {
            continue;
        };

        if kind == "(fetch)" {
            remotes.insert(name.to_owned(), url.to_owned());
        }
    }

    remotes
        .into_iter()
        .map(|(name, url)| GitRemote { name, url })
        .collect()
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}
