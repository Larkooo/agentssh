use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Check if `path` is inside a git repository.
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["-C", &path.to_string_lossy(), "rev-parse", "--git-dir"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a worktree inside `<repo-root>/.agentssh/worktrees/<short-id>/`
/// on a new branch `agentssh/<short-id>` from HEAD.
/// Returns the worktree path.
pub fn create_worktree(repo_path: &Path) -> Result<PathBuf> {
    // Find the repo root
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
        .context("failed to run git rev-parse")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("not a git repository: {}", stderr.trim());
    }

    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());

    // Generate a short timestamp-based ID
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    let worktree_dir = root.join(".agentssh").join("worktrees");
    std::fs::create_dir_all(&worktree_dir)
        .with_context(|| format!("failed to create {}", worktree_dir.display()))?;

    let worktree_path = worktree_dir.join(&id);
    let branch_name = format!("agentssh/{id}");

    let output = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            "-b",
            &branch_name,
        ])
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(worktree_path)
}

/// Clone `url` into `dest_dir/<repo-name>/`. Returns the clone path.
/// Repo name is derived from the URL (last path segment minus .git).
pub fn clone_repo(url: &str, dest_dir: &Path) -> Result<PathBuf> {
    let repo_name = parse_repo_name(url)?;
    let clone_path = dest_dir.join(&repo_name);

    let output = Command::new("git")
        .args(["clone", url, &clone_path.to_string_lossy()])
        .output()
        .context("failed to run git clone")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone failed: {}", stderr.trim());
    }

    Ok(clone_path)
}

/// Extract repo name from a git URL.
/// Strips trailing `.git` and takes the last path segment.
fn parse_repo_name(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last_segment = trimmed
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("cannot parse repo name from URL: {url}"))?;

    let name = last_segment.strip_suffix(".git").unwrap_or(last_segment);
    if name.is_empty() {
        anyhow::bail!("cannot parse repo name from URL: {url}");
    }

    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_name_https() {
        assert_eq!(
            parse_repo_name("https://github.com/user/repo.git").unwrap(),
            "repo"
        );
    }

    #[test]
    fn parse_repo_name_https_no_git() {
        assert_eq!(
            parse_repo_name("https://github.com/user/repo").unwrap(),
            "repo"
        );
    }

    #[test]
    fn parse_repo_name_ssh() {
        assert_eq!(
            parse_repo_name("git@github.com:user/repo.git").unwrap(),
            "repo"
        );
    }

    #[test]
    fn parse_repo_name_trailing_slash() {
        assert_eq!(
            parse_repo_name("https://github.com/user/repo/").unwrap(),
            "repo"
        );
    }

    #[test]
    fn parse_repo_name_empty_errors() {
        assert!(parse_repo_name("").is_err());
    }

    #[test]
    fn is_git_repo_false_for_tmp() {
        assert!(!is_git_repo(Path::new("/tmp")));
    }
}
