use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};

use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    backend::{ContainerBackend, ContainerRuntime, ExecRequest},
    cli::{Cli, Commands, EnvironmentName, RunCommand},
    config::{
        CONFIG_FILE_NAME, ConfigError, LoadedConfig, ProjectConfig, default_init_config,
        parse_timeout,
    },
    env_model::{ResolvedEnvironment, ResolvedUser},
    error::OrodruinError,
    state::ContainerSummary,
};

#[derive(Debug, Serialize)]
pub(crate) struct ListEntry {
    pub env: String,
    pub container: String,
    pub state: String,
    pub running: bool,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectReport<'a> {
    pub env: &'a str,
    pub container: &'a str,
    pub image: &'a str,
    pub workdir: &'a str,
    pub container_exists: bool,
    pub resolved: &'a ResolvedEnvironment,
    pub inspect: Option<Value>,
}

pub(crate) fn prompt_strategy(auto_yes: bool) -> fn() -> Result<bool, OrodruinError> {
    if auto_yes {
        auto_yes_prompt
    } else {
        prompt_to_start_container_system
    }
}

pub(crate) fn auto_yes_prompt() -> Result<bool, OrodruinError> {
    Ok(true)
}

pub(crate) fn print_json<T: Serialize>(value: &T) -> Result<(), OrodruinError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| OrodruinError::Message(error.to_string()))?
    );
    Ok(())
}

pub(crate) fn init_command() -> Result<(), OrodruinError> {
    let cwd = std::env::current_dir()?;
    let path = cwd.join(CONFIG_FILE_NAME);
    if path.exists() {
        return Err(OrodruinError::Message(format!(
            "{CONFIG_FILE_NAME} already exists at {}",
            path.display()
        )));
    }

    let project_name = cwd
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    fs::write(&path, default_init_config(project_name)?)?;
    println!("created {}", path.display());
    Ok(())
}

pub(crate) fn load_config(explicit_path: Option<&Path>) -> Result<LoadedConfig, OrodruinError> {
    let cwd = std::env::current_dir()?;
    Ok(ProjectConfig::load_from(&cwd, explicit_path)?)
}

pub(crate) fn configured_timeout_for_cli(cli: &Cli) -> Result<Option<Duration>, OrodruinError> {
    if let Some(timeout) = cli.timeout {
        return Ok(Some(timeout));
    }

    let loaded = match &cli.command {
        Commands::Create(_)
        | Commands::Enter(_)
        | Commands::Run(_)
        | Commands::List(_)
        | Commands::Rm(_)
        | Commands::Inspect(_)
        | Commands::Doctor(_) => Some(load_config(cli.config.as_deref())?),
        Commands::Init | Commands::Completions(_) | Commands::Version => None,
        _ => try_load_config(cli.config.as_deref())?,
    };

    let Some(loaded) = loaded else {
        return Ok(None);
    };

    let configured = match &cli.command {
        Commands::Create(environment) | Commands::Rm(environment) => {
            resolve_environment(&loaded, environment)?.timeout
        }
        Commands::Inspect(command) => resolve_environment_by_name(&loaded, &command.env)?.timeout,
        Commands::Enter(command) => {
            resolve_optional_environment(&loaded, command.env.as_deref(), "enter")?.timeout
        }
        Commands::Run(command) => {
            resolve_optional_environment(&loaded, command.env.as_deref(), "run")?.timeout
        }
        Commands::List(_)
        | Commands::Doctor(_)
        | Commands::Pull(_)
        | Commands::Images(_)
        | Commands::Rmi(_)
        | Commands::Ps(_)
        | Commands::Logs(_)
        | Commands::Build(_)
        | Commands::Copy(_)
        | Commands::Login(_)
        | Commands::Logout(_)
        | Commands::Image(_)
        | Commands::Container(_)
        | Commands::Registry(_)
        | Commands::Volume(_)
        | Commands::Network(_)
        | Commands::Builder(_)
        | Commands::System(_)
        | Commands::Machine(_) => loaded.config.project.default_timeout.clone(),
        Commands::Init | Commands::Completions(_) | Commands::Version => None,
    };

    configured
        .as_deref()
        .map(parse_timeout)
        .transpose()
        .map_err(OrodruinError::Message)
}

pub(crate) fn try_load_config(
    explicit_path: Option<&Path>,
) -> Result<Option<LoadedConfig>, OrodruinError> {
    let cwd = std::env::current_dir()?;
    match ProjectConfig::load_from(&cwd, explicit_path) {
        Ok(loaded) => Ok(Some(loaded)),
        Err(ConfigError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn resolve_environment(
    loaded: &LoadedConfig,
    environment: &EnvironmentName,
) -> Result<ResolvedEnvironment, OrodruinError> {
    resolve_environment_by_name(loaded, &environment.env)
}

pub(crate) fn resolve_optional_environment<'a>(
    loaded: &'a LoadedConfig,
    env_name: Option<&'a str>,
    command_name: &str,
) -> Result<ResolvedEnvironment, OrodruinError> {
    let env_name = match env_name {
        Some(env_name) => env_name,
        None => resolve_default_environment_name(loaded, command_name)?,
    };

    resolve_environment_by_name(loaded, env_name)
}

pub(crate) fn resolve_default_environment_name<'a>(
    loaded: &'a LoadedConfig,
    command_name: &str,
) -> Result<&'a str, OrodruinError> {
    if let Some(default_env) = loaded.config.project.default_env.as_deref() {
        return Ok(default_env);
    }

    if loaded.config.envs.len() == 1 {
        return Ok(loaded
            .config
            .envs
            .keys()
            .next()
            .expect("single environment checked above")
            .as_str());
    }

    Err(OrodruinError::Message(format!(
        "`orodruin {command_name}` needs an environment name; set project.default_env in {} or define only one environment",
        loaded.path.display(),
    )))
}

pub(crate) fn resolve_environment_by_name(
    loaded: &LoadedConfig,
    env_name: &str,
) -> Result<ResolvedEnvironment, OrodruinError> {
    let config = loaded.config.envs.get(env_name).ok_or_else(|| {
        OrodruinError::Message(format!(
            "environment `{}` is not defined in {}",
            env_name,
            loaded.path.display()
        ))
    })?;

    Ok(ResolvedEnvironment::resolve(
        &loaded.root,
        &loaded.config,
        env_name,
        config,
    ))
}

pub(crate) fn resolve_run_command(
    resolved: &ResolvedEnvironment,
    run: RunCommand,
) -> Result<Vec<String>, OrodruinError> {
    if !run.command.is_empty() {
        return Ok(run.command);
    }
    resolved.default_command.clone().ok_or_else(|| {
        OrodruinError::Message(format!(
            "environment `{}` has no default command; provide one after `--`",
            resolved.environment_name
        ))
    })
}

pub(crate) fn materialize_environment_for_runtime_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    resolved: &ResolvedEnvironment,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    materialize_environment(backend, resolved)
}

pub(crate) fn container_exists_for_runtime_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    container_name: &str,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<bool, OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    container_exists(backend, container_name)
}

pub(crate) fn list_environment_entries_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<Vec<ListEntry>, OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    let containers = backend.list_all()?;
    loaded
        .config
        .envs
        .iter()
        .map(|(name, config)| {
            let resolved = ResolvedEnvironment::resolve(&loaded.root, &loaded.config, name, config);
            let summary = containers
                .iter()
                .find(|summary| summary.matches(&resolved.container_name));
            Ok(summary_entry(name, &resolved.container_name, summary))
        })
        .collect()
}

pub(crate) fn list_environments_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    let entries = list_environment_entries_with_prompt(backend, runtime, loaded, prompt)?;
    for entry in entries {
        println!("{}\t{}\t{}", entry.env, entry.container, entry.state);
    }
    Ok(())
}

pub(crate) fn materialize_environment(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<(), OrodruinError> {
    let current = backend
        .list_all()?
        .into_iter()
        .find(|summary| summary.matches(&resolved.container_name));

    match current {
        Some(summary) if summary.running => Ok(()),
        Some(_) => {
            backend.start(&resolved.container_name)?;
            Ok(())
        }
        None => {
            if let Some(build) = &resolved.build {
                backend.build_image(build)?;
            }
            backend.create(resolved)?;
            Ok(())
        }
    }
}

pub(crate) fn ensure_container_system_running_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    if !runtime.manages_system_lifecycle() {
        return Ok(());
    }

    if backend.system_running()? {
        return Ok(());
    }

    if prompt()? {
        backend.start_system()?;
        return Ok(());
    }

    Err(OrodruinError::Message(
        "container system is not running; start it with `container system start` or rerun with `--yes` to allow automatic startup".into(),
    ))
}

pub(crate) fn prompt_to_start_container_system() -> Result<bool, OrodruinError> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    prompt_to_start_container_system_with_io(&mut stdin, &mut stderr)
}

pub(crate) fn prompt_to_start_container_system_with_io<R, W>(
    stdin: &mut R,
    stderr: &mut W,
) -> Result<bool, OrodruinError>
where
    R: BufRead,
    W: Write,
{
    loop {
        write!(
            stderr,
            "container system is not running. Start it now? [y/N] "
        )?;
        stderr.flush()?;

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            return Ok(false);
        }

        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" | "" => return Ok(false),
            _ => {
                writeln!(stderr, "please answer y or n")?;
            }
        }
    }
}

pub(crate) fn ensure_container_user(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<(), OrodruinError> {
    backend.exec(
        &resolved.container_name,
        &ExecRequest {
            workdir: Some("/".into()),
            env: vec![
                ("ORODRUIN_HOST_USER".into(), resolved.user.username.clone()),
                ("ORODRUIN_HOST_UID".into(), resolved.user.uid.to_string()),
                ("ORODRUIN_HOST_GID".into(), resolved.user.gid.to_string()),
                ("ORODRUIN_HOST_HOME".into(), resolved.user.home.clone()),
            ],
            command: vec![
                "/bin/sh".into(),
                "-lc".into(),
                bootstrap_user_script().into(),
            ],
            interactive: false,
            user: Some(ResolvedUser {
                username: "root".into(),
                uid: 0,
                gid: 0,
                home: "/root".into(),
            }),
        },
    )?;
    Ok(())
}

pub(crate) fn parse_extra_env(vars: &[String]) -> Result<Vec<(String, String)>, OrodruinError> {
    let mut result = Vec::with_capacity(vars.len());
    for var in vars {
        let (key, value) = var.split_once('=').ok_or_else(|| {
            OrodruinError::Message(format!("invalid --env value `{var}`; expected KEY=VALUE"))
        })?;
        if key.is_empty() {
            return Err(OrodruinError::Message(
                "--env: variable name must not be empty".into(),
            ));
        }
        result.push((key.to_string(), value.to_string()));
    }
    Ok(result)
}

pub(crate) fn exec_environment(
    resolved: &ResolvedEnvironment,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = resolved
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    env.push(("HOME".into(), resolved.user.home.clone()));
    env.push(("USER".into(), resolved.user.username.clone()));
    env.push(("LOGNAME".into(), resolved.user.username.clone()));

    // Apply CLI --env overrides (highest priority, overrides resolved and HOME/USER/LOGNAME)
    for (key, value) in extra_env {
        if let Some(pos) = env.iter().position(|(k, _)| k == key) {
            env[pos] = (key.clone(), value.clone());
        } else {
            env.push((key.clone(), value.clone()));
        }
    }

    env
}

pub(crate) fn bootstrap_user_script() -> &'static str {
    r#"set -eu

username="${ORODRUIN_HOST_USER}"
uid="${ORODRUIN_HOST_UID}"
gid="${ORODRUIN_HOST_GID}"
home="${ORODRUIN_HOST_HOME}"

lookup_group_by_gid() {
    awk -F: -v gid="$1" '$3 == gid { print $1; exit }' /etc/group
}

lookup_user_by_name() {
    awk -F: -v name="$1" '$1 == name { print $3 ":" $4; exit }' /etc/passwd
}

lookup_user_by_uid() {
    awk -F: -v uid="$1" '$3 == uid { print $1; exit }' /etc/passwd
}

group_name="$(lookup_group_by_gid "$gid")"
if [ -z "$group_name" ]; then
    if command -v groupadd >/dev/null 2>&1; then
        groupadd -g "$gid" "$username"
        group_name="$username"
    elif command -v addgroup >/dev/null 2>&1; then
        addgroup -g "$gid" "$username"
        group_name="$username"
    else
        echo "missing groupadd/addgroup for gid ${gid}" >&2
        exit 1
    fi
fi

existing_user="$(lookup_user_by_name "$username")"
if [ -n "$existing_user" ]; then
    existing_uid="${existing_user%%:*}"
    existing_gid="${existing_user##*:}"
    if [ "$existing_uid" != "$uid" ] || [ "$existing_gid" != "$gid" ]; then
        echo "user ${username} exists with uid:gid ${existing_uid}:${existing_gid}, expected ${uid}:${gid}" >&2
        exit 1
    fi
else
    uid_owner="$(lookup_user_by_uid "$uid")"
    if [ -n "$uid_owner" ] && [ "$uid_owner" != "$username" ]; then
        username="$uid_owner"
        existing_user="$(lookup_user_by_name "$username")"
        existing_gid="${existing_user##*:}"
        if [ "$existing_gid" != "$gid" ]; then
            echo "uid ${uid} already owned by ${uid_owner} with gid ${existing_gid}, expected ${gid}" >&2
            exit 1
        fi
    else
        if command -v useradd >/dev/null 2>&1; then
            if useradd -K UID_MIN=0 -K UID_MAX=60000 -m -d "$home" -u "$uid" -g "$gid" -s /bin/sh "$username"; then
                :
            else
                useradd -m -d "$home" -u "$uid" -g "$gid" -s /bin/sh "$username"
            fi
        elif command -v adduser >/dev/null 2>&1; then
            adduser -D -h "$home" -u "$uid" -G "$group_name" "$username"
        else
            echo "missing useradd/adduser for ${username}" >&2
            exit 1
        fi
    fi
fi

mkdir -p "$home"
chown "$uid:$gid" "$home"
"#
}

pub(crate) fn container_exists(
    backend: &dyn ContainerBackend,
    container_name: &str,
) -> Result<bool, OrodruinError> {
    Ok(backend
        .list_all()?
        .iter()
        .any(|summary| summary.matches(container_name)))
}

pub(crate) fn inspect_environment_with_prompt<'a>(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    resolved: &'a ResolvedEnvironment,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<InspectReport<'a>, OrodruinError> {
    let container_exists = container_exists_for_runtime_with_prompt(
        backend,
        runtime,
        &resolved.container_name,
        prompt,
    )?;
    let inspect = if container_exists {
        backend.inspect_raw(&resolved.container_name)?
    } else {
        None
    };

    Ok(InspectReport {
        env: &resolved.environment_name,
        container: &resolved.container_name,
        image: &resolved.image,
        workdir: &resolved.workdir,
        container_exists,
        resolved,
        inspect,
    })
}

pub(crate) fn summary_entry(
    name: &str,
    container_name: &str,
    summary: Option<&ContainerSummary>,
) -> ListEntry {
    let (state, running, exists) = match summary {
        Some(summary) if summary.running => (String::from("running"), true, true),
        Some(summary) => (
            summary.status.as_deref().unwrap_or("created").to_string(),
            false,
            true,
        ),
        None => (String::from("not-created"), false, false),
    };

    ListEntry {
        env: name.to_string(),
        container: container_name.to_string(),
        state,
        running,
        exists,
    }
}

pub(crate) fn print_inspect_report(report: &InspectReport<'_>) {
    println!("env: {}", report.env);
    println!("container: {}", report.container);
    println!("image: {}", report.image);
    println!("workdir: {}", report.workdir);
    if !report.container_exists {
        println!("container not created");
        return;
    }

    match &report.inspect {
        Some(value) => println!(
            "{}",
            to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        ),
        None => println!("container not created"),
    }
}
