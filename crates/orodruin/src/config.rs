use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CONFIG_FILE_NAME: &str = "orodruin.toml";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectConfig {
    #[serde(default)]
    pub project: ProjectMetadata,
    pub envs: BTreeMap<String, EnvironmentConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectMetadata {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentConfig {
    pub image: Option<String>,
    pub build: Option<BuildConfig>,
    pub container_name: Option<String>,
    pub project_mount: Option<String>,
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub preserve_env: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    pub shell: Option<Vec<String>>,
    pub startup_command: Option<Vec<String>>,
    pub default_command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BuildConfig {
    pub context: String,
    pub file: Option<String>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MountConfig {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub root: PathBuf,
    pub path: PathBuf,
    pub config: ProjectConfig,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not find {CONFIG_FILE_NAME} from {0}")]
    NotFound(String),
    #[error("failed to read config at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid config: {0}")]
    Validation(String),
}

impl ProjectConfig {
    pub fn load_from(
        start_dir: &Path,
        explicit_path: Option<&Path>,
    ) -> Result<LoadedConfig, ConfigError> {
        let path = match explicit_path {
            Some(path) => path.to_path_buf(),
            None => find_config_path(start_dir)?,
        };
        let root = fs::canonicalize(path.parent().ok_or_else(|| {
            ConfigError::Validation("config path has no parent directory".into())
        })?)
        .unwrap_or_else(|_| {
            path.parent()
                .expect("config path parent checked above")
                .to_path_buf()
        });
        let content = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let config =
            toml::from_str::<ProjectConfig>(&content).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
        config.validate()?;
        Ok(LoadedConfig { root, path, config })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.envs.is_empty() {
            return Err(ConfigError::Validation(
                "define at least one environment under [envs.<name>]".into(),
            ));
        }

        for (name, env) in &self.envs {
            let has_image = env.image.is_some();
            let has_build = env.build.is_some();
            if has_image == has_build {
                return Err(ConfigError::Validation(format!(
                    "environment `{name}` must define exactly one of `image` or `build`"
                )));
            }

            if let Some(project_mount) = &env.project_mount {
                validate_absolute_path(name, "project_mount", project_mount)?;
            }
            if let Some(workdir) = &env.workdir {
                validate_absolute_path(name, "workdir", workdir)?;
            }
            if let Some(build) = &env.build
                && build.context.trim().is_empty()
            {
                return Err(ConfigError::Validation(format!(
                    "environment `{name}` has an empty build context"
                )));
            }

            let mut preserve_env = HashSet::new();
            for key in &env.preserve_env {
                if key.trim().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "environment `{name}` contains an empty preserve_env entry"
                    )));
                }
                preserve_env.insert(key);
            }
            for mount in &env.mounts {
                if mount.source.trim().is_empty() || mount.target.trim().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "environment `{name}` mounts must define non-empty source and target"
                    )));
                }
                validate_absolute_path(name, "mount target", &mount.target)?;
            }
            if let Some(shell) = &env.shell
                && shell.is_empty()
            {
                return Err(ConfigError::Validation(format!(
                    "environment `{name}` shell must contain at least one token"
                )));
            }
            if let Some(startup_command) = &env.startup_command
                && startup_command.is_empty()
            {
                return Err(ConfigError::Validation(format!(
                    "environment `{name}` startup_command must contain at least one token"
                )));
            }
            if let Some(default_command) = &env.default_command
                && default_command.is_empty()
            {
                return Err(ConfigError::Validation(format!(
                    "environment `{name}` default_command must contain at least one token"
                )));
            }
        }

        Ok(())
    }
}

pub fn default_init_config(project_name: &str) -> String {
    format!(
        r#"[project]
name = "{project_name}"

[envs.dev]
image = "ubuntu:latest"
project_mount = "/workspace/{project_name}"
workdir = "/workspace/{project_name}"
preserve_env = ["SSH_AUTH_SOCK"]
shell = ["/bin/bash"]
startup_command = ["sleep", "infinity"]
"#
    )
}

fn validate_absolute_path(env_name: &str, field: &str, path: &str) -> Result<(), ConfigError> {
    if !path.starts_with('/') {
        return Err(ConfigError::Validation(format!(
            "environment `{env_name}` {field} must be an absolute path"
        )));
    }
    Ok(())
}

fn find_config_path(start_dir: &Path) -> Result<PathBuf, ConfigError> {
    for directory in start_dir.ancestors() {
        let candidate = directory.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(ConfigError::NotFound(start_dir.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_valid_config() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [envs.dev]
                image = "ubuntu:latest"
            "#,
        )
        .unwrap();

        config.validate().unwrap();
        assert!(config.envs.contains_key("dev"));
    }

    #[test]
    fn parses_multiple_environments() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [envs.dev]
                image = "ubuntu:latest"

                [envs.ci]
                build = { context = ".", file = "Containerfile" }
            "#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.envs.len(), 2);
    }

    #[test]
    fn rejects_invalid_mount_target() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [envs.dev]
                image = "ubuntu:latest"
                mounts = [{ source = ".", target = "workspace" }]
            "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("mount target must be an absolute path")
        );
    }

    #[test]
    fn rejects_missing_image_or_build() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [envs.dev]
                workdir = "/workspace/app"
            "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must define exactly one of `image` or `build`")
        );
    }
}
