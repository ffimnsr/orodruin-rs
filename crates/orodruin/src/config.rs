use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
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
    pub default_env: Option<String>,
    pub default_timeout: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentConfig {
    pub image: Option<String>,
    pub build: Option<BuildConfig>,
    pub container_name: Option<String>,
    pub project_mount: Option<String>,
    pub workdir: Option<String>,
    pub timeout: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub preserve_env: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    #[serde(default)]
    pub env_files: Vec<String>,
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
    #[error("failed to serialize default config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("invalid config: {0}")]
    Validation(String),
}

pub fn parse_timeout(value: &str) -> Result<Duration, String> {
    let (number, unit) = match value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit())
    {
        Some((index, _)) => (&value[..index], &value[index..]),
        None => (value, "s"),
    };
    if number.is_empty() {
        return Err("duration must start with a positive integer".into());
    }
    let amount = number
        .parse::<u64>()
        .map_err(|_| format!("invalid duration `{value}`"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" | "" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount.saturating_mul(60))),
        "h" => Ok(Duration::from_secs(amount.saturating_mul(60 * 60))),
        _ => Err(format!(
            "invalid duration unit in `{value}`; use ms, s, m, or h"
        )),
    }
}

impl ProjectConfig {
    pub fn locate_path(
        start_dir: &Path,
        explicit_path: Option<&Path>,
    ) -> Result<PathBuf, ConfigError> {
        match explicit_path {
            Some(path) => Ok(path.to_path_buf()),
            None => find_config_path(start_dir),
        }
    }

    pub fn load_path(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let path = path.to_path_buf();
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

    pub fn load_from(
        start_dir: &Path,
        explicit_path: Option<&Path>,
    ) -> Result<LoadedConfig, ConfigError> {
        let path = Self::locate_path(start_dir, explicit_path)?;
        Self::load_path(&path)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.envs.is_empty() {
            return Err(ConfigError::Validation(
                "define at least one environment under [envs.<name>]".into(),
            ));
        }

        if let Some(default_env) = self.project.default_env.as_deref() {
            if default_env.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "project.default_env must not be empty".into(),
                ));
            }
            if !self.envs.contains_key(default_env) {
                return Err(ConfigError::Validation(format!(
                    "project.default_env `{default_env}` is not defined under [envs.<name>]"
                )));
            }
        }
        if let Some(default_timeout) = self.project.default_timeout.as_deref() {
            validate_timeout("project.default_timeout", default_timeout)?;
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
            if let Some(timeout) = env.timeout.as_deref() {
                validate_timeout(&format!("environment `{name}` timeout"), timeout)?;
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

            for env_file in &env.env_files {
                if env_file.trim().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "environment `{name}` contains an empty env_files entry"
                    )));
                }
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

pub fn default_init_config(project_name: &str) -> Result<String, ConfigError> {
    let mut envs = BTreeMap::new();
    envs.insert(
        "dev".to_string(),
        EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: Some(format!("/workspace/{project_name}")),
            workdir: Some(format!("/workspace/{project_name}")),
            timeout: None,
            env: BTreeMap::new(),
            preserve_env: vec!["SSH_AUTH_SOCK".into()],
            mounts: vec![],
            env_files: vec![],
            shell: Some(vec!["/bin/bash".into()]),
            startup_command: Some(vec!["sleep".into(), "infinity".into()]),
            default_command: None,
        },
    );

    toml::to_string_pretty(&ProjectConfig {
        project: ProjectMetadata {
            name: Some(project_name.to_string()),
            default_env: None,
            default_timeout: None,
        },
        envs,
    })
    .map_err(ConfigError::from)
}

fn validate_timeout(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "{field} must not be empty"
        )));
    }
    parse_timeout(value)
        .map(|_| ())
        .map_err(ConfigError::Validation)
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

    #[test]
    fn rejects_invalid_default_timeout() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [project]
                default_timeout = "4d"

                [envs.dev]
                image = "ubuntu:latest"
            "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("invalid duration unit"));
    }

    #[test]
    fn rejects_unknown_default_env() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [project]
                default_env = "ci"

                [envs.dev]
                image = "ubuntu:latest"
            "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("project.default_env `ci` is not defined")
        );
    }

    #[test]
    fn rejects_empty_env_files_entry() {
        let config: ProjectConfig = toml::from_str(
            r#"
                [envs.dev]
                image = "ubuntu:latest"
                env_files = [""]
            "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("empty env_files entry"));
    }

    #[test]
    fn default_init_config_serializes_escaped_project_name() {
        let rendered = default_init_config("demo \"quoted\"").unwrap();
        let config: ProjectConfig = toml::from_str(&rendered).unwrap();

        assert_eq!(config.project.name.as_deref(), Some("demo \"quoted\""));
        let env = config.envs.get("dev").unwrap();
        assert_eq!(
            env.project_mount.as_deref(),
            Some("/workspace/demo \"quoted\"")
        );
        assert_eq!(env.workdir.as_deref(), Some("/workspace/demo \"quoted\""));
        assert_eq!(env.timeout, None);
        assert_eq!(
            env.shell.as_deref(),
            Some([String::from("/bin/bash")].as_slice())
        );
    }
}
