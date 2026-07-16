use std::{
    collections::BTreeMap,
    env,
    ffi::CStr,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::config::{BuildConfig, EnvironmentConfig, ProjectConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedEnvironment {
    pub project_name: String,
    pub environment_name: String,
    pub container_name: String,
    pub image: String,
    pub project_root: PathBuf,
    pub project_mount: String,
    pub workdir: String,
    pub env: BTreeMap<String, String>,
    pub mounts: Vec<ResolvedMount>,
    pub user: ResolvedUser,
    pub shell: Vec<String>,
    pub startup_command: Vec<String>,
    pub default_command: Option<Vec<String>>,
    pub timeout: Option<String>,
    pub build: Option<ResolvedBuild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedBuild {
    pub context: PathBuf,
    pub file: Option<PathBuf>,
    pub tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedMount {
    pub source: PathBuf,
    pub target: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedUser {
    pub username: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

impl ResolvedEnvironment {
    pub fn resolve(
        project_root: &Path,
        project_config: &ProjectConfig,
        env_name: &str,
        config: &EnvironmentConfig,
    ) -> Self {
        let project_name = project_config
            .project
            .name
            .clone()
            .unwrap_or_else(|| fallback_project_name(project_root));
        let project_slug = slugify(&project_name);
        let project_mount = config
            .project_mount
            .clone()
            .unwrap_or_else(|| format!("/workspace/{project_slug}"));
        let workdir = config
            .workdir
            .clone()
            .unwrap_or_else(|| project_mount.clone());
        let container_name = config.container_name.clone().unwrap_or_else(|| {
            format!(
                "orodruin-{}-{}",
                slugify(&project_name),
                stable_suffix(project_root, env_name)
            )
        });

        let mut env_map = BTreeMap::new();

        // 1. Load env_files (lowest priority)
        for env_file_path in &config.env_files {
            let path = resolve_relative_path(project_root, env_file_path);
            if let Ok(file_env) = parse_env_file(&path) {
                for (key, value) in file_env {
                    env_map.entry(key).or_insert(value);
                }
            }
        }

        // 2. Config env overrides env_files
        for (key, value) in &config.env {
            env_map.insert(key.clone(), value.clone());
        }

        // 3. Preserve env fills gaps (does not override config.env)
        for key in &config.preserve_env {
            if let Ok(value) = env::var(key) {
                env_map.entry(key.clone()).or_insert(value);
            }
        }

        let mut mounts = Vec::with_capacity(config.mounts.len() + 1);
        mounts.push(ResolvedMount {
            source: project_root.to_path_buf(),
            target: project_mount.clone(),
            readonly: false,
        });
        mounts.extend(config.mounts.iter().map(|mount| ResolvedMount {
            source: resolve_relative_path(project_root, &mount.source),
            target: mount.target.clone(),
            readonly: mount.readonly,
        }));

        let build = config
            .build
            .as_ref()
            .map(|build| resolve_build(project_root, &container_name, build));
        let user = resolve_current_user();

        Self {
            project_name,
            environment_name: env_name.to_string(),
            container_name,
            image: config
                .image
                .clone()
                .or_else(|| build.as_ref().map(|value| value.tag.clone()))
                .expect("validated config always provides image or build"),
            project_root: project_root.to_path_buf(),
            project_mount,
            workdir,
            env: env_map,
            mounts,
            user,
            shell: config
                .shell
                .clone()
                .unwrap_or_else(|| vec!["/bin/sh".to_string()]),
            startup_command: config
                .startup_command
                .clone()
                .unwrap_or_else(|| vec!["sleep".to_string(), "infinity".to_string()]),
            default_command: config.default_command.clone(),
            timeout: config
                .timeout
                .clone()
                .or_else(|| project_config.project.default_timeout.clone()),
            build,
        }
    }
}

fn resolve_build(project_root: &Path, container_name: &str, build: &BuildConfig) -> ResolvedBuild {
    let context = resolve_relative_path(project_root, &build.context);
    let file = build
        .file
        .as_ref()
        .map(|value| resolve_relative_path(project_root, value));
    let tag = build
        .tag
        .clone()
        .unwrap_or_else(|| format!("{container_name}:dev"));
    ResolvedBuild { context, file, tag }
}

fn resolve_relative_path(project_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn fallback_project_name(project_root: &Path) -> String {
    project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project")
        .to_string()
}

fn stable_suffix(project_root: &Path, env_name: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in project_root
        .display()
        .to_string()
        .bytes()
        .chain(std::iter::once(b':'))
        .chain(env_name.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for character in value.chars() {
        let next = if character.is_ascii_alphanumeric() {
            Some(character.to_ascii_lowercase())
        } else {
            None
        };
        if let Some(character) = next {
            slug.push(character);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn resolve_current_user() -> ResolvedUser {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let username = current_username(uid).unwrap_or_else(|| uid.to_string());
    let home = if uid == 0 {
        "/root".to_string()
    } else {
        format!("/home/{username}")
    };
    ResolvedUser {
        username,
        uid,
        gid,
        home,
    }
}

pub(crate) fn parse_env_file(path: &Path) -> Result<BTreeMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut map = BTreeMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Strip optional "export " prefix
        let entry = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();

        // Find the first '=' to split key and value
        if let Some(eq_pos) = entry.find('=') {
            let key = entry[..eq_pos].trim().to_string();
            if key.is_empty() {
                continue;
            }
            let mut value = entry[eq_pos + 1..].trim().to_string();

            // Strip surrounding quotes if matching
            let value_len = value.len();
            if value_len >= 2 {
                let first = value.chars().next().unwrap();
                let last = value.chars().last().unwrap();
                if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
                    value = value[1..value_len - 1].to_string();
                }
            }

            map.insert(key, value);
        }
    }

    Ok(map)
}

fn current_username(uid: u32) -> Option<String> {
    let mut buffer = vec![0; 4096];
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status == 0 && !result.is_null() {
        let passwd = unsafe { passwd.assume_init() };
        return Some(
            unsafe { CStr::from_ptr(passwd.pw_name) }
                .to_string_lossy()
                .into_owned(),
        );
    }

    env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("LOGNAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectMetadata;

    #[test]
    fn derives_stable_container_names() {
        let project = ProjectConfig {
            project: ProjectMetadata {
                name: Some("My App".into()),
                default_env: None,
                default_timeout: None,
            },
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            timeout: None,
            env: Default::default(),
            preserve_env: vec![],
            mounts: vec![],
            env_files: vec![],
            shell: None,
            startup_command: None,
            default_command: None,
        };

        let first = ResolvedEnvironment::resolve(Path::new("/tmp/example"), &project, "dev", &env);
        let second = ResolvedEnvironment::resolve(Path::new("/tmp/example"), &project, "dev", &env);

        assert_eq!(first.container_name, second.container_name);
        assert!(first.container_name.starts_with("orodruin-my-app-"));
        assert!(!first.user.username.is_empty());
        assert_eq!(first.user.home, format!("/home/{}", first.user.username));
    }

    #[test]
    fn applies_project_mount_and_workdir_defaults() {
        let project = ProjectConfig {
            project: ProjectMetadata::default(),
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            timeout: None,
            env: Default::default(),
            preserve_env: vec![],
            mounts: vec![],
            env_files: vec![],
            shell: None,
            startup_command: None,
            default_command: None,
        };

        let resolved = ResolvedEnvironment::resolve(Path::new("/tmp/app"), &project, "dev", &env);

        assert_eq!(resolved.project_mount, "/workspace/app");
        assert_eq!(resolved.workdir, "/workspace/app");
        assert_eq!(resolved.mounts[0].source, Path::new("/tmp/app"));
        assert_eq!(resolved.user.uid, unsafe { libc::getuid() });
        assert_eq!(resolved.user.gid, unsafe { libc::getgid() });
    }

    #[test]
    fn parse_env_file_reads_key_value_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            r#"# comment
                FOO=bar
                export BAZ=qux
                EMPTY=
                QUOTED="hello world"
                SINGLE='single quoted'
            "#,
        )
        .unwrap();

        let env = parse_env_file(&path).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(env.get("BAZ").map(String::as_str), Some("qux"));
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(env.get("QUOTED").map(String::as_str), Some("hello world"));
        assert_eq!(env.get("SINGLE").map(String::as_str), Some("single quoted"));
        assert_eq!(env.len(), 5);
    }

    #[test]
    fn parse_env_file_skips_empty_and_commented_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "\n\n  \n# just a comment\n#another one\n").unwrap();

        let env = parse_env_file(&path).unwrap();
        assert!(env.is_empty());
    }

    #[test]
    fn parse_env_file_handles_missing_file_gracefully() {
        let path = Path::new("/tmp/nonexistent-file-for-test-12345.env");
        let result = parse_env_file(path);
        assert!(result.is_err());
    }

    #[test]
    fn env_file_values_are_overridden_by_explicit_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join(".env");
        std::fs::write(&env_file, "MY_VAR=from_file\nOTHER=also_from_file\n").unwrap();

        let project = ProjectConfig {
            project: ProjectMetadata::default(),
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            timeout: None,
            env: BTreeMap::from([(String::from("MY_VAR"), String::from("from_config"))]),
            preserve_env: vec![],
            mounts: vec![],
            env_files: vec![env_file.display().to_string()],
            shell: None,
            startup_command: None,
            default_command: None,
        };

        let resolved = ResolvedEnvironment::resolve(dir.path(), &project, "dev", &env);

        // Config env overrides env_file
        assert_eq!(
            resolved.env.get("MY_VAR").map(String::as_str),
            Some("from_config")
        );
        // Env_file values that aren't in config.env are preserved
        assert_eq!(
            resolved.env.get("OTHER").map(String::as_str),
            Some("also_from_file")
        );
    }

    #[test]
    fn merges_preserved_and_explicit_env() {
        let project = ProjectConfig {
            project: ProjectMetadata::default(),
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            timeout: None,
            env: BTreeMap::from([(String::from("LANG"), String::from("C.UTF-8"))]),
            preserve_env: vec!["SHOULD_NOT_EXIST".into()],
            mounts: vec![],
            env_files: vec![],
            shell: None,
            startup_command: None,
            default_command: Some(vec!["cargo".into(), "test".into()]),
        };

        let resolved = ResolvedEnvironment::resolve(Path::new("/tmp/app"), &project, "dev", &env);

        assert_eq!(
            resolved.env.get("LANG").map(String::as_str),
            Some("C.UTF-8")
        );
        assert_eq!(
            resolved.default_command,
            Some(vec![String::from("cargo"), String::from("test")])
        );
        assert_eq!(
            resolved.startup_command,
            vec![String::from("sleep"), String::from("infinity")]
        );
    }
}
