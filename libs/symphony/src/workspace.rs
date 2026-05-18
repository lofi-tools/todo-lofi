use crate::domain::*;
use crate::error::SymphonyError::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::io::{self, Write};
use std::time::Duration;
use crate::config::HooksConfig;

/// Manage workspaces for issues.
pub struct WorkspaceManager {
    root: PathBuf,
    hooks: HooksConfig,
}

impl WorkspaceManager {
    /// Create a new workspace manager.
    pub fn new(root: PathBuf, hooks: HooksConfig) -> Self {
        Self { root, hooks }
    }
    
    /// Get or create a workspace for an issue.
    pub fn get_or_create_workspace(&self, issue: &Issue) -> Result<PathBuf> {
        let workspace_key = self.sanitize_identifier(&issue.identifier);
        let workspace_path = self.root.join(&workspace_key);
        
        // Check if workspace already exists
        let workspace_exists = workspace_path.is_dir();
        
        // Create directory if it doesn't exist
        if !workspace_exists {
            fs::create_dir_all(&workspace_path)
                .map_err(|e| WorkspaceError { source: Box::new(e) })?;
            
            // Run after_create hook if this is a newly created workspace
            if let Some(hook) = &self.hooks.after_create {
                self.run_hook(hook, &workspace_path, "after_create")?;
            }
        }
        
        Ok(workspace_path)
    }
    
    /// Remove a workspace for an issue.
    pub fn remove_workspace(&self, issue: &Issue) -> Result<()> {
        let workspace_key = self.sanitize_identifier(&issue.identifier);
        let workspace_path = self.root.join(&workspace_key);
        
        // Check if workspace exists
        if workspace_path.is_dir() {
            // Run before_remove hook
            if let Some(hook) = &self.hooks.before_remove {
                self.run_hook(hook, &workspace_path, "before_remove")?;
            }
            
            // Remove the workspace directory
            fs::remove_dir_all(&workspace_path)
                .map_err(|e| WorkspaceError { source: Box::new(e) })?;
        }
        
        Ok(())
    }
    
    /// Validate that a path is within the workspace root.
    pub fn validate_path(&self, path: &Path) -> Result<()> {
        let canonical_path = dunce::canonicalize(path)
            .map_err(|e| WorkspaceError { source: Box::new(e) })?;
        let canonical_root = dunce::canonicalize(&self.root)
            .map_err(|e| WorkspaceError { source: Box::new(e) })?;
        
        if !canonical_path.starts_with(&canonical_root) {
            return Err(InvalidWorkspacePath {
                path: canonical_path.to_string_lossy().into_owned(),
                root: canonical_root.to_string_lossy().into_owned(),
            });
        }
        
        Ok(())
    }
    
    /// Sanitize an identifier to create a valid directory name.
    fn sanitize_identifier(&self, identifier: &str) -> String {
        identifier
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
    
    /// Run a shell hook in the workspace directory.
    fn run_hook(&self, hook: &str, workspace_path: &Path, hook_name: &str) -> Result<()> {
        // For simplicity, we'll use bash -c on Unix-like systems
        // In a production implementation, you might want to handle Windows differently
        let status = Command::new("sh")
            .arg("-lc")
            .arg(hook)
            .current_dir(workspace_path)
            .status()
            .map_err(|e| WorkspaceError { source: Box::new(e) })?;
        
        if !status.success() {
            return Err(WorkspaceError {
                source: Box::new(io::Error::new(
                    io::ErrorKind::Other,
                    format!("{} hook failed with exit code: {}", hook_name, status.code().unwrap_or(-1)),
                ))
            });
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::config::HooksConfig;
    
    #[test]
    fn test_sanitize_identifier() {
        let manager = WorkspaceManager::new(PathBuf::from("/tmp"), HooksConfig::default());
        
        assert_eq!(manager.sanitize_identifier("ABC-123"), "ABC-123");
        assert_eq!(manager.sanitize_identifier("test.feature"), "test.feature");
        assert_eq!(manager.sanitize_identifier("test feature"), "test_feature");
        assert_eq!(manager.sanitize_identifier("test@feature"), "test_feature");
        assert_eq!(manager.sanitize_identifier("test/feature"), "test_feature");
    }
    
    #[test]
    fn test_get_or_create_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let hooks = HooksConfig::default();
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks);
        
        let issue = Issue {
            id: "test-1".to_string(),
            identifier: "TEST-123".to_string(),
            title: "Test Issue".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };
        
        // First call should create the workspace
        let workspace_path = manager.get_or_create_workspace(&issue).unwrap();
        assert!(workspace_path.exists());
        assert!(workspace_path.is_dir());
        assert_eq!(workspace_path.file_name().unwrap(), "TEST-123");
        
        // Second call should return the same path
        let workspace_path2 = manager.get_or_create_workspace(&issue).unwrap();
        assert_eq!(workspace_path, workspace_path2);
    }
    
    #[test]
    fn test_remove_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let hooks = HooksConfig::default();
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks);
        
        let issue = Issue {
            id: "test-1".to_string(),
            identifier: "TEST-123".to_string(),
            title: "Test Issue".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };
        
        // Create workspace
        let workspace_path = manager.get_or_create_workspace(&issue).unwrap();
        assert!(workspace_path.exists());
        
        // Remove workspace
        manager.remove_workspace(&issue).unwrap();
        assert!(!workspace_path.exists());
    }
    
    #[test]
    fn test_validate_path() {
        let temp_dir = TempDir::new().unwrap();
        let hooks = HooksConfig::default();
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks);
        
        // Valid path inside root
        let valid_path = temp_dir.path().join("subdir");
        fs::create_dir_all(&valid_path).unwrap();
        assert!(manager.validate_path(&valid_path).is_ok());
        
        // Invalid path outside root
        let invalid_path = std::env::temp_dir();
        assert!(manager.validate_path(&invalid_path).is_err());
    }
}