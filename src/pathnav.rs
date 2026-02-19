use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    SelectCurrent,
    CreateDirectory,
    CloneFromUrl,
    Parent,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Browser {
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivateResult {
    Selected(PathBuf),
    StartCreateDirectory,
    StartCloneFromUrl,
    ChangedDirectory,
}

impl Browser {
    pub fn new(start: PathBuf) -> Result<Self> {
        let mut browser = Self {
            cwd: start,
            entries: Vec::new(),
            selected: 0,
        };
        browser.refresh()?;
        Ok(browser)
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn next(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1) % self.entries.len();
    }

    pub fn previous(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        if self.selected == 0 {
            self.selected = self.entries.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn activate_selected(&mut self) -> Result<ActivateResult> {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return Ok(ActivateResult::Selected(self.cwd.clone()));
        };

        match entry.kind {
            EntryKind::SelectCurrent => Ok(ActivateResult::Selected(self.cwd.clone())),
            EntryKind::CreateDirectory => Ok(ActivateResult::StartCreateDirectory),
            EntryKind::CloneFromUrl => Ok(ActivateResult::StartCloneFromUrl),
            EntryKind::Parent | EntryKind::Directory => {
                self.cwd = entry.path;
                self.refresh()?;
                Ok(ActivateResult::ChangedDirectory)
            }
        }
    }

    pub fn create_directory(&mut self, name: &str) -> Result<PathBuf> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("directory name cannot be empty"));
        }
        if trimmed.contains('/') || trimmed.contains('\\') {
            return Err(anyhow::anyhow!(
                "directory name must be a single path segment"
            ));
        }

        let new_path = self.cwd.join(trimmed);
        fs::create_dir_all(&new_path)
            .with_context(|| format!("failed creating directory {}", new_path.display()))?;
        self.cwd = new_path.clone();
        self.refresh()?;
        Ok(new_path)
    }

    pub fn refresh(&mut self) -> Result<()> {
        let mut dirs: Vec<(String, PathBuf)> = fs::read_dir(&self.cwd)
            .with_context(|| format!("failed reading directory {}", self.cwd.display()))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() {
                    return None;
                }

                let name = entry.file_name().to_string_lossy().to_string();
                Some((name, path))
            })
            .collect();

        dirs.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));

        let mut entries = Vec::new();
        entries.push(Entry {
            kind: EntryKind::SelectCurrent,
            label: format!("Use {}", self.cwd.display()),
            path: self.cwd.clone(),
        });
        entries.push(Entry {
            kind: EntryKind::CreateDirectory,
            label: "Create directory here...".to_owned(),
            path: self.cwd.clone(),
        });
        entries.push(Entry {
            kind: EntryKind::CloneFromUrl,
            label: "Clone from URL...".to_owned(),
            path: self.cwd.clone(),
        });

        if let Some(parent) = self.cwd.parent() {
            entries.push(Entry {
                kind: EntryKind::Parent,
                label: "..".to_owned(),
                path: parent.to_path_buf(),
            });
        }

        for (name, path) in dirs {
            entries.push(Entry {
                kind: EntryKind::Directory,
                label: name,
                path,
            });
        }

        self.entries = entries;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn browser_has_use_entry_first() {
        let root = std::env::temp_dir().join(format!(
            "agentssh-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time ok")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("child_a")).expect("create child_a");
        fs::create_dir_all(root.join("child_b")).expect("create child_b");

        let browser = Browser::new(root.clone()).expect("browser create");

        assert_eq!(browser.entries[0].kind, EntryKind::SelectCurrent);
        assert_eq!(browser.entries[1].kind, EntryKind::CreateDirectory);
        assert!(browser.entries.iter().any(|e| e.label == ".."));
        assert!(browser.entries.iter().any(|e| e.label == "child_a"));
        assert!(browser.entries.iter().any(|e| e.label == "child_b"));

        fs::remove_dir_all(root).expect("cleanup root");
    }

    #[test]
    fn activate_parent_changes_directory() {
        let root = std::env::temp_dir().join(format!(
            "agentssh-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time ok")
                .as_nanos()
        ));
        let child = root.join("child");
        fs::create_dir_all(&child).expect("create child");

        let mut browser = Browser::new(child.clone()).expect("browser create");
        let parent_index = browser
            .entries()
            .iter()
            .position(|entry| entry.kind == EntryKind::Parent)
            .expect("parent entry");

        while browser.selected() != parent_index {
            browser.next();
        }

        let result = browser.activate_selected().expect("activate parent");
        assert_eq!(result, ActivateResult::ChangedDirectory);
        assert_eq!(browser.cwd(), root.as_path());

        fs::remove_dir_all(root).expect("cleanup root");
    }

    #[test]
    fn create_directory_moves_into_new_path() {
        let root = std::env::temp_dir().join(format!(
            "agentssh-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time ok")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create root");

        let mut browser = Browser::new(root.clone()).expect("browser create");
        let created = browser
            .create_directory("new_workspace")
            .expect("create dir");

        assert_eq!(created, root.join("new_workspace"));
        assert_eq!(browser.cwd(), root.join("new_workspace").as_path());

        fs::remove_dir_all(root).expect("cleanup root");
    }
}
