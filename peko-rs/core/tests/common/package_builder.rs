//! Local snapshot fixtures using the production packager.
#![allow(dead_code)]
use peko_core::principal::config::PrincipalConfig;
use peko_core::registry::packaging::{PrincipalExportOptions, PrincipalPackager};
use peko_identity::{did::DIDScope, Identity};
use std::path::PathBuf;

pub struct PrincipalPackageBuilder {
    name: String,
    skills: Vec<String>,
}
impl PrincipalPackageBuilder {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            skills: Vec::new(),
        }
    }
    pub fn with_skill(mut self, id: &str) -> Self {
        self.skills.push(id.into());
        self
    }
    pub async fn build(self) -> anyhow::Result<PathBuf> {
        let temp = tempfile::tempdir()?;
        let config: PrincipalConfig = toml::from_str(&format!("name = {:?}\n", self.name))?;
        let identity = Identity::generate(DIDScope::Local, Some("fixture"))?;
        for id in &self.skills {
            let dir = temp.path().join("skills").join(id);
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(
                dir.join("SKILL.md"),
                "---\nname: fixture-skill\ndescription: Test skill\n---\nFixture\n",
            )
            .await?;
        }
        let path = temp.path().join(format!("{}.peko", self.name));
        PrincipalPackager::new(config, identity)
            .with_workspace_dir(temp.path())
            .export(PrincipalExportOptions {
                output_path: Some(path.display().to_string()),
                ..Default::default()
            })
            .await?;
        let _ = temp.keep();
        Ok(path)
    }
}
