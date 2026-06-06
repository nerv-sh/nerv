pub mod backup;
pub mod desktop_entry;
pub mod error;
pub mod file;
#[cfg(target_os = "linux")]
pub mod gnome_extension;
pub mod shell;
// PLAN.md v0.6 §0.2 strips IDE/desktop integration paths
// (input_method / intellij / vscode / ssh). Sources removed —
// see git history for the original Fig implementations.

use async_trait::async_trait;
pub use backup::backup_file;
pub use error::{Error, Result};
pub use file::FileIntegration;

#[async_trait]
pub trait Integration {
    fn describe(&self) -> String;
    async fn install(&self) -> Result<()>;
    async fn uninstall(&self) -> Result<()>;
    async fn is_installed(&self) -> Result<()>;

    /// Apply any migrations, this can be called at any time so do not do anything too destructive
    async fn migrate(&self) -> Result<()> {
        Ok(())
    }
}
