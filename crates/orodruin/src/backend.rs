use std::{
    io,
    process::{Command, Stdio},
};

use serde_json::Value;
use thiserror::Error;

use crate::{
    env_model::{ResolvedBuild, ResolvedEnvironment, ResolvedMount, ResolvedUser},
    state::{ContainerSummary, StateError, parse_inspect_output, parse_list_output},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    AppleContainer,
    Podman,
}

impl ContainerRuntime {
    pub fn from_os(os: &str) -> Option<Self> {
        match os {
            "macos" => Some(Self::AppleContainer),
            "linux" => Some(Self::Podman),
            _ => None,
        }
    }

    pub fn program(self) -> &'static str {
        match self {
            Self::AppleContainer => "container",
            Self::Podman => "podman",
        }
    }

    pub fn manages_system_lifecycle(self) -> bool {
        matches!(self, Self::AppleContainer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn render(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub workdir: Option<String>,
    pub env: Vec<(String, String)>,
    pub command: Vec<String>,
    pub interactive: bool,
    pub user: Option<ResolvedUser>,
}

pub trait ContainerBackend {
    fn list_all(&self) -> Result<Vec<ContainerSummary>, BackendError>;
    fn inspect_raw(&self, container_name: &str) -> Result<Option<Value>, BackendError>;
    fn system_running(&self) -> Result<bool, BackendError>;
    fn start_system(&self) -> Result<(), BackendError>;
    fn build_image(&self, build: &ResolvedBuild) -> Result<(), BackendError>;
    fn create(&self, environment: &ResolvedEnvironment) -> Result<(), BackendError>;
    fn start(&self, container_name: &str) -> Result<(), BackendError>;
    fn exec(&self, container_name: &str, request: &ExecRequest) -> Result<(), BackendError>;
    fn delete(&self, container_name: &str) -> Result<(), BackendError>;
    fn run_command(&self, step: &str, spec: &CommandSpec) -> Result<(), BackendError>;
}

pub struct ContainerCliBackend {
    debug: bool,
    runtime: ContainerRuntime,
}

impl ContainerCliBackend {
    pub fn new(debug: bool, runtime: ContainerRuntime) -> Self {
        Self { debug, runtime }
    }

    pub fn build_list_spec(runtime: ContainerRuntime) -> CommandSpec {
        match runtime {
            ContainerRuntime::AppleContainer => CommandSpec {
                program: runtime.program().into(),
                args: vec![
                    "list".into(),
                    "--all".into(),
                    "--format".into(),
                    "json".into(),
                ],
            },
            ContainerRuntime::Podman => CommandSpec {
                program: runtime.program().into(),
                args: vec!["ps".into(), "--all".into(), "--format".into(), "json".into()],
            },
        }
    }

    pub fn build_inspect_spec(runtime: ContainerRuntime, container_name: &str) -> CommandSpec {
        CommandSpec {
            program: runtime.program().into(),
            args: vec!["inspect".into(), container_name.into()],
        }
    }

    pub fn build_system_status_spec(runtime: ContainerRuntime) -> CommandSpec {
        CommandSpec {
            program: runtime.program().into(),
            args: vec![
                "system".into(),
                "status".into(),
                "--format".into(),
                "json".into(),
            ],
        }
    }

    pub fn build_system_start_spec(runtime: ContainerRuntime) -> CommandSpec {
        CommandSpec {
            program: runtime.program().into(),
            args: vec!["system".into(), "start".into()],
        }
    }

    pub fn build_build_spec(runtime: ContainerRuntime, build: &ResolvedBuild) -> CommandSpec {
        let mut args = vec!["build".into(), "-t".into(), build.tag.clone()];
        if let Some(file) = &build.file {
            args.push("-f".into());
            args.push(file.display().to_string());
        }
        args.push(build.context.display().to_string());
        CommandSpec {
            program: runtime.program().into(),
            args,
        }
    }

    pub fn build_create_spec(
        runtime: ContainerRuntime,
        environment: &ResolvedEnvironment,
    ) -> CommandSpec {
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            environment.container_name.clone(),
            "--workdir".into(),
            environment.workdir.clone(),
        ];
        append_env_args(&mut args, environment.env.iter());
        append_mount_args(&mut args, &environment.mounts);
        args.push(environment.image.clone());
        args.extend(environment.startup_command.iter().cloned());
        CommandSpec {
            program: runtime.program().into(),
            args,
        }
    }

    pub fn build_start_spec(runtime: ContainerRuntime, container_name: &str) -> CommandSpec {
        CommandSpec {
            program: runtime.program().into(),
            args: vec!["start".into(), container_name.into()],
        }
    }

    pub fn build_exec_spec(
        runtime: ContainerRuntime,
        container_name: &str,
        request: &ExecRequest,
    ) -> CommandSpec {
        let mut args = vec!["exec".into()];
        if request.interactive {
            args.push("-i".into());
            args.push("-t".into());
        }
        append_user_args(&mut args, request.user.as_ref());
        if let Some(workdir) = &request.workdir {
            args.push("--workdir".into());
            args.push(workdir.clone());
        }
        append_env_args(
            &mut args,
            request.env.iter().map(|(key, value)| (key, value)),
        );
        args.push(container_name.into());
        args.extend(request.command.iter().cloned());
        CommandSpec {
            program: runtime.program().into(),
            args,
        }
    }

    pub fn build_delete_spec(runtime: ContainerRuntime, container_name: &str) -> CommandSpec {
        let args = match runtime {
            ContainerRuntime::AppleContainer => {
                vec!["delete".into(), "--force".into(), container_name.into()]
            }
            ContainerRuntime::Podman => vec!["rm".into(), "--force".into(), container_name.into()],
        };
        CommandSpec {
            program: runtime.program().into(),
            args,
        }
    }

    pub fn build_passthrough_spec(runtime: ContainerRuntime, args: Vec<String>) -> CommandSpec {
        CommandSpec {
            program: runtime.program().into(),
            args,
        }
    }

    fn run_captured(&self, step: &str, spec: &CommandSpec) -> Result<String, BackendError> {
        if self.debug {
            eprintln!("debug: {}", spec.render());
        }
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| BackendError::Spawn {
                step: step.to_string(),
                command: spec.render(),
                source,
            })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(BackendError::CommandFailed {
            step: step.to_string(),
            command: spec.render(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn run_interactive(&self, step: &str, spec: &CommandSpec) -> Result<(), BackendError> {
        if self.debug {
            eprintln!("debug: {}", spec.render());
        }
        let status = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|source| BackendError::Spawn {
                step: step.to_string(),
                command: spec.render(),
                source,
            })?;
        if status.success() {
            return Ok(());
        }
        Err(BackendError::CommandFailed {
            step: step.to_string(),
            command: spec.render(),
            status: status.code(),
            stderr: String::new(),
        })
    }
}

impl ContainerBackend for ContainerCliBackend {
    fn list_all(&self) -> Result<Vec<ContainerSummary>, BackendError> {
        let output = self.run_captured("list containers", &Self::build_list_spec(self.runtime))?;
        parse_list_output(&output).map_err(BackendError::from)
    }

    fn inspect_raw(&self, container_name: &str) -> Result<Option<Value>, BackendError> {
        let output = self.run_captured(
            "inspect container",
            &Self::build_inspect_spec(self.runtime, container_name),
        )?;
        Ok(parse_inspect_output(&output))
    }

    fn system_running(&self) -> Result<bool, BackendError> {
        if !self.runtime.manages_system_lifecycle() {
            return Ok(true);
        }

        match self.run_captured(
            "check system status",
            &Self::build_system_status_spec(self.runtime),
        ) {
            Ok(output) => Ok(system_status_reports_running(&output)),
            Err(BackendError::CommandFailed { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn start_system(&self) -> Result<(), BackendError> {
        if !self.runtime.manages_system_lifecycle() {
            return Ok(());
        }

        self.run_interactive("start system", &Self::build_system_start_spec(self.runtime))
    }

    fn build_image(&self, build: &ResolvedBuild) -> Result<(), BackendError> {
        self.run_interactive("build image", &Self::build_build_spec(self.runtime, build))
    }

    fn create(&self, environment: &ResolvedEnvironment) -> Result<(), BackendError> {
        self.run_interactive(
            "create and start container",
            &Self::build_create_spec(self.runtime, environment),
        )
    }

    fn start(&self, container_name: &str) -> Result<(), BackendError> {
        self.run_interactive(
            "start container",
            &Self::build_start_spec(self.runtime, container_name),
        )
    }

    fn exec(&self, container_name: &str, request: &ExecRequest) -> Result<(), BackendError> {
        self.run_interactive(
            "exec command",
            &Self::build_exec_spec(self.runtime, container_name, request),
        )
    }

    fn delete(&self, container_name: &str) -> Result<(), BackendError> {
        self.run_interactive(
            "delete container",
            &Self::build_delete_spec(self.runtime, container_name),
        )
    }

    fn run_command(&self, step: &str, spec: &CommandSpec) -> Result<(), BackendError> {
        self.run_interactive(step, spec)
    }
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("failed to spawn `{command}` while attempting to {step}: {source}")]
    Spawn {
        step: String,
        command: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "`{command}` failed while attempting to {step}; exit status: {status:?}; stderr: {stderr}"
    )]
    CommandFailed {
        step: String,
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error(transparent)]
    State(#[from] StateError),
}

impl BackendError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::CommandFailed {
                status: Some(code), ..
            } => *code,
            _ => 1,
        }
    }
}

fn append_env_args<'a>(
    args: &mut Vec<String>,
    values: impl Iterator<Item = (&'a String, &'a String)>,
) {
    for (key, value) in values {
        args.push("--env".into());
        args.push(format!("{key}={value}"));
    }
}

fn append_mount_args(args: &mut Vec<String>, mounts: &[ResolvedMount]) {
    for mount in mounts {
        args.push("--mount".into());
        let mut value = format!(
            "type=bind,source={},target={}",
            mount.source.display(),
            mount.target
        );
        if mount.readonly {
            value.push_str(",readonly");
        }
        args.push(value);
    }
}

fn append_user_args(args: &mut Vec<String>, user: Option<&ResolvedUser>) {
    if let Some(user) = user {
        args.push("--user".into());
        args.push(format!("{}:{}", user.uid, user.gid));
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_./:=,".contains(character))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn system_status_reports_running(output: &str) -> bool {
    serde_json::from_str::<Value>(output)
        .ok()
        .is_some_and(|value| value_contains_apiserver(&value))
}

fn value_contains_apiserver(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.iter().any(|(key, candidate)| {
                matches!(key.as_str(), "appName" | "component" | "name")
                    && candidate.as_str() == Some("container-apiserver")
            }) {
                return true;
            }

            map.values().any(value_contains_apiserver)
        }
        Value::Array(values) => values.iter().any(value_contains_apiserver),
        Value::String(candidate) => candidate == "container-apiserver",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::*;

    fn sample_environment() -> ResolvedEnvironment {
        ResolvedEnvironment {
            project_name: "app".into(),
            environment_name: "dev".into(),
            container_name: "orodruin-app-123".into(),
            image: "ubuntu:latest".into(),
            project_root: PathBuf::from("/tmp/app"),
            project_mount: "/workspace/app".into(),
            workdir: "/workspace/app".into(),
            env: BTreeMap::from([(String::from("LANG"), String::from("C.UTF-8"))]),
            mounts: vec![
                ResolvedMount {
                    source: PathBuf::from("/tmp/app"),
                    target: "/workspace/app".into(),
                    readonly: false,
                },
                ResolvedMount {
                    source: PathBuf::from("/tmp/cache"),
                    target: "/cache".into(),
                    readonly: true,
                },
            ],
            user: ResolvedUser {
                username: "dev".into(),
                uid: 501,
                gid: 20,
                home: "/home/dev".into(),
            },
            shell: vec!["/bin/bash".into()],
            startup_command: vec!["sleep".into(), "infinity".into()],
            default_command: None,
            build: None,
        }
    }

    #[test]
    fn create_emits_expected_container_command() {
        let environment = sample_environment();
        let spec = ContainerCliBackend::build_create_spec(ContainerRuntime::AppleContainer, &environment);

        assert_eq!(spec.program, "container");
        assert_eq!(spec.args[0], "run");
        assert_eq!(spec.args[1], "-d");
        assert!(spec.args.contains(&"--name".to_string()));
        assert!(
            spec.args
                .contains(&"type=bind,source=/tmp/cache,target=/cache,readonly".to_string())
        );
        assert!(spec.args.ends_with(&[
            String::from("ubuntu:latest"),
            String::from("sleep"),
            String::from("infinity")
        ]));
    }

    #[test]
    fn exec_targets_container_workdir_and_command() {
        let spec = ContainerCliBackend::build_exec_spec(
            ContainerRuntime::AppleContainer,
            "orodruin-app-123",
            &ExecRequest {
                workdir: Some("/workspace/app".into()),
                env: vec![(String::from("FOO"), String::from("bar"))],
                command: vec!["cargo".into(), "test".into()],
                interactive: false,
                user: Some(ResolvedUser {
                    username: "dev".into(),
                    uid: 501,
                    gid: 20,
                    home: "/home/dev".into(),
                }),
            },
        );

        assert_eq!(spec.args[0], "exec");
        assert!(spec.args.contains(&"--user".to_string()));
        assert!(spec.args.contains(&"501:20".to_string()));
        assert!(spec.args.contains(&"--workdir".to_string()));
        assert!(spec.args.contains(&"orodruin-app-123".to_string()));
        assert_eq!(spec.args.last().map(String::as_str), Some("test"));
    }

    #[test]
    fn render_wraps_command_context() {
        let spec = CommandSpec {
            program: "container".into(),
            args: vec!["exec".into(), "demo".into(), "echo hello".into()],
        };

        assert_eq!(spec.render(), "container exec demo 'echo hello'");
    }

    #[test]
    fn passthrough_spec_uses_container_binary() {
        let spec = ContainerCliBackend::build_passthrough_spec(
            ContainerRuntime::AppleContainer,
            vec![
            "image".into(),
            "pull".into(),
            "alpine:latest".into(),
        ]);

        assert_eq!(spec.program, "container");
        assert_eq!(spec.args, ["image", "pull", "alpine:latest"]);
    }

    #[test]
    fn podman_specs_use_linux_command_shape() {
        let list_spec = ContainerCliBackend::build_list_spec(ContainerRuntime::Podman);
        let delete_spec =
            ContainerCliBackend::build_delete_spec(ContainerRuntime::Podman, "demo");

        assert_eq!(list_spec.program, "podman");
        assert_eq!(list_spec.args, ["ps", "--all", "--format", "json"]);
        assert_eq!(delete_spec.program, "podman");
        assert_eq!(delete_spec.args, ["rm", "--force", "demo"]);
    }
}
