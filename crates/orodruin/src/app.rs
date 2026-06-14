use std::{
    ffi::OsString,
    fs,
    net::IpAddr,
    path::Path,
    process::{Command as ProcessCommand, Stdio},
};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use serde_json::{Value, to_string_pretty};

use crate::{
    backend::{AppleContainerBackend, BackendError, CommandSpec, ContainerBackend, ExecRequest},
    cli::{
        BuilderCommands, Cli, Commands, CompletionsCommand, ContainerCommands, EnvironmentName,
        ImageCommands, MachineCommands, OptionalPassthroughArgs, RegistryCommands,
        RequiredPassthroughArgs, ResourceCommands, RunCommand,
        SystemCommands,
    },
    config::{CONFIG_FILE_NAME, LoadedConfig, ProjectConfig, default_init_config},
    env_model::ResolvedEnvironment,
    error::OrodruinError,
    state::ContainerSummary,
};

struct PassthroughInvocation {
    step: String,
    prefix: Vec<String>,
    args: Vec<String>,
}

pub fn run<I, T>(args: I) -> Result<(), OrodruinError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let backend = AppleContainerBackend::new(cli.debug);
    run_with_backend(cli, &backend)
}

fn run_with_backend(cli: Cli, backend: &dyn ContainerBackend) -> Result<(), OrodruinError> {
    match cli.command {
        Commands::Init => init_command()?,
        Commands::Create(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment(backend, &resolved)?;
            println!("ready {}", resolved.container_name);
        }
        Commands::Enter(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_optional_environment(&loaded, environment.env.as_deref(), "enter")?;
            materialize_environment(backend, &resolved)?;
            ensure_container_user(backend, &resolved)?;
            match backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved),
                    command: resolved.shell.clone(),
                    interactive: true,
                    user: Some(resolved.user.clone()),
                },
            ) {
                Ok(()) => {}
                Err(BackendError::CommandFailed { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Commands::Ssh(command) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_optional_environment(&loaded, command.env.as_deref(), "ssh")?;
            materialize_environment(backend, &resolved)?;
            ensure_container_user(backend, &resolved)?;
            let target = resolve_ssh_target(backend, &resolved)?;
            let spec = build_ssh_spec(&resolved, &target);
            if command.print {
                println!("{}", spec.render());
            } else {
                run_host_command(&spec)?;
            }
        }
        Commands::Run(run) => {
            let loaded = load_config(cli.config.as_deref())?;
            let environment = EnvironmentName {
                env: run.env.clone(),
            };
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment(backend, &resolved)?;
            let command = resolve_run_command(&resolved, run)?;
            ensure_container_user(backend, &resolved)?;
            backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved),
                    command,
                    interactive: false,
                    user: Some(resolved.user.clone()),
                },
            )?;
        }
        Commands::List => {
            let loaded = load_config(cli.config.as_deref())?;
            let containers = backend.list_all()?;
            for (name, config) in &loaded.config.envs {
                let resolved =
                    ResolvedEnvironment::resolve(&loaded.root, &loaded.config, name, config);
                let summary = containers
                    .iter()
                    .find(|summary| summary.matches(&resolved.container_name));
                print_summary(name, &resolved.container_name, summary);
            }
        }
        Commands::Rm(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            if !container_exists(backend, &resolved.container_name)? {
                return Err(OrodruinError::Message(format!(
                    "environment `{}` is not created",
                    resolved.environment_name
                )));
            }
            backend.delete(&resolved.container_name)?;
            println!("removed {}", resolved.container_name);
        }
        Commands::Inspect(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            println!("env: {}", resolved.environment_name);
            println!("container: {}", resolved.container_name);
            println!("image: {}", resolved.image);
            println!("workdir: {}", resolved.workdir);
            if !container_exists(backend, &resolved.container_name)? {
                println!("container not created");
                return Ok(());
            }
            let inspect = backend.inspect_raw(&resolved.container_name)?;
            match inspect {
                Some(value) => println!(
                    "{}",
                    to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                ),
                None => println!("container not created"),
            }
        }
        Commands::Pull(args) => run_passthrough(backend, passthrough("pull image", &["image", "pull"], args))?,
        Commands::Images(args) => run_passthrough(backend, passthrough("list images", &["image", "list"], args))?,
        Commands::Rmi(args) => run_passthrough(backend, passthrough("remove image", &["image", "delete"], args))?,
        Commands::Ps(args) => run_passthrough(backend, passthrough("list containers", &["list"], args))?,
        Commands::Logs(args) => run_passthrough(backend, passthrough("show container logs", &["logs"], args))?,
        Commands::Build(args) => run_passthrough(backend, passthrough("build image", &["build"], args))?,
        Commands::Copy(args) => run_passthrough(backend, passthrough("copy files", &["copy"], args))?,
        Commands::Login(args) => run_passthrough(backend, passthrough("login registry", &["registry", "login"], args))?,
        Commands::Logout(args) => run_passthrough(backend, passthrough("logout registry", &["registry", "logout"], args))?,
        Commands::Image(command) => run_passthrough(backend, image_passthrough(command))?,
        Commands::Container(command) => run_passthrough(backend, container_passthrough(command))?,
        Commands::Registry(command) => run_passthrough(backend, registry_passthrough(command))?,
        Commands::Volume(command) => run_passthrough(backend, resource_passthrough("volume", command))?,
        Commands::Network(command) => run_passthrough(backend, resource_passthrough("network", command))?,
        Commands::Builder(command) => run_passthrough(backend, builder_passthrough(command))?,
        Commands::System(command) => run_passthrough(backend, system_passthrough(command))?,
        Commands::Machine(command) => run_passthrough(backend, machine_passthrough(command))?,
        Commands::Completions(command) => print!("{}", render_completions(command)?),
        Commands::Version => println!("{}", crate::build_info::render()),
    }

    Ok(())
}

fn run_passthrough(
    backend: &dyn ContainerBackend,
    invocation: PassthroughInvocation,
) -> Result<(), OrodruinError> {
    let mut command = invocation.prefix;
    command.extend(invocation.args);
    let spec = AppleContainerBackend::build_passthrough_spec(command);
    backend.run_command(&invocation.step, &spec)?;
    Ok(())
}

fn passthrough(
    step: &'static str,
    prefix: &'static [&'static str],
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    PassthroughInvocation {
        step: step.to_string(),
        prefix: prefix.iter().map(|value| (*value).to_string()).collect(),
        args: args.into(),
    }
}

fn render_completions(command: CompletionsCommand) -> Result<String, OrodruinError> {
    let mut cli = Cli::command();
    let mut output = Vec::new();
    let name = cli.get_name().to_string();
    generate(command.shell, &mut cli, name, &mut output);
    String::from_utf8(output).map_err(|error| OrodruinError::Message(error.to_string()))
}

fn resource_passthrough(
    resource: &str,
    command: ResourceCommands,
) -> PassthroughInvocation {
    match command {
        ResourceCommands::List(args) => passthrough_owned(format!("list {resource}s"), vec![resource, "list"], args),
        ResourceCommands::Create(args) => passthrough_owned(format!("create {resource}"), vec![resource, "create"], args),
        ResourceCommands::Inspect(args) => passthrough_owned(format!("inspect {resource}"), vec![resource, "inspect"], args),
        ResourceCommands::Prune(args) => passthrough_owned(format!("prune {resource}s"), vec![resource, "prune"], args),
        ResourceCommands::Remove(args) => passthrough_owned(format!("remove {resource}"), vec![resource, "delete"], args),
    }
}

fn passthrough_owned(
    step: String,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    PassthroughInvocation {
        step,
        prefix: prefix.into_iter().map(str::to_string).collect(),
        args: args.into(),
    }
}

fn image_passthrough(command: ImageCommands) -> PassthroughInvocation {
    match command {
        ImageCommands::Pull(args) => passthrough("pull image", &["image", "pull"], args),
        ImageCommands::List(args) => passthrough("list images", &["image", "list"], args),
        ImageCommands::Inspect(args) => passthrough("inspect image", &["image", "inspect"], args),
        ImageCommands::Load(args) => passthrough("load image", &["image", "load"], args),
        ImageCommands::Remove(args) => passthrough("remove image", &["image", "delete"], args),
        ImageCommands::Push(args) => passthrough("push image", &["image", "push"], args),
        ImageCommands::Prune(args) => passthrough("prune images", &["image", "prune"], args),
        ImageCommands::Save(args) => passthrough("save image", &["image", "save"], args),
        ImageCommands::Tag(args) => passthrough("tag image", &["image", "tag"], args),
    }
}

fn container_passthrough(command: ContainerCommands) -> PassthroughInvocation {
    match command {
        ContainerCommands::Create(args) => passthrough("create container", &["create"], args),
        ContainerCommands::Exec(args) => passthrough("exec container command", &["exec"], args),
        ContainerCommands::Export(args) => passthrough("export container", &["export"], args),
        ContainerCommands::Kill(args) => passthrough("kill container", &["kill"], args),
        ContainerCommands::List(args) => passthrough("list containers", &["list"], args),
        ContainerCommands::Inspect(args) => passthrough("inspect container", &["inspect"], args),
        ContainerCommands::Logs(args) => passthrough("show container logs", &["logs"], args),
        ContainerCommands::Prune(args) => passthrough("prune containers", &["prune"], args),
        ContainerCommands::Remove(args) => passthrough("remove container", &["delete"], args),
        ContainerCommands::Run(args) => passthrough("run container", &["run"], args),
        ContainerCommands::Start(args) => passthrough("start container", &["start"], args),
        ContainerCommands::Stats(args) => passthrough("container stats", &["stats"], args),
        ContainerCommands::Stop(args) => passthrough("stop container", &["stop"], args),
    }
}

fn registry_passthrough(command: RegistryCommands) -> PassthroughInvocation {
    match command {
        RegistryCommands::List(args) => passthrough("list registries", &["registry", "list"], args),
        RegistryCommands::Login(args) => passthrough("login registry", &["registry", "login"], args),
        RegistryCommands::Logout(args) => passthrough("logout registry", &["registry", "logout"], args),
    }
}

fn builder_passthrough(command: BuilderCommands) -> PassthroughInvocation {
    match command {
        BuilderCommands::Remove(args) => passthrough("remove builder", &["builder", "delete"], args),
        BuilderCommands::Start(args) => passthrough("start builder", &["builder", "start"], args),
        BuilderCommands::Status(args) => passthrough("builder status", &["builder", "status"], args),
        BuilderCommands::Stop(args) => passthrough("stop builder", &["builder", "stop"], args),
    }
}

fn system_passthrough(command: SystemCommands) -> PassthroughInvocation {
    match command {
        SystemCommands::Df(args) => passthrough("system df", &["system", "df"], args),
        SystemCommands::Dns(args) => passthrough("system dns", &["system", "dns"], args),
        SystemCommands::Kernel(args) => passthrough("system kernel", &["system", "kernel"], args),
        SystemCommands::Logs(args) => passthrough("system logs", &["system", "logs"], args),
        SystemCommands::Property(args) => passthrough("system property", &["system", "property"], args),
        SystemCommands::Start(args) => passthrough("start system", &["system", "start"], args),
        SystemCommands::Status(args) => passthrough("system status", &["system", "status"], args),
        SystemCommands::Stop(args) => passthrough("stop system", &["system", "stop"], args),
        SystemCommands::Version(args) => passthrough("system version", &["system", "version"], args),
    }
}

fn machine_passthrough(command: MachineCommands) -> PassthroughInvocation {
    match command {
        MachineCommands::Create(args) => passthrough("create machine", &["machine", "create"], args),
        MachineCommands::Inspect(args) => passthrough("inspect machine", &["machine", "inspect"], args),
        MachineCommands::List(args) => passthrough("list machines", &["machine", "list"], args),
        MachineCommands::Logs(args) => passthrough("machine logs", &["machine", "logs"], args),
        MachineCommands::Remove(args) => passthrough("remove machine", &["machine", "delete"], args),
        MachineCommands::Run(args) => passthrough("run machine command", &["machine", "run"], args),
        MachineCommands::Set(args) => passthrough("set machine", &["machine", "set"], args),
        MachineCommands::SetDefault(args) => passthrough("set default machine", &["machine", "set-default"], args),
        MachineCommands::Stop(args) => passthrough("stop machine", &["machine", "stop"], args),
    }
}

impl From<OptionalPassthroughArgs> for Vec<String> {
    fn from(value: OptionalPassthroughArgs) -> Self {
        value.args
    }
}

impl From<RequiredPassthroughArgs> for Vec<String> {
    fn from(value: RequiredPassthroughArgs) -> Self {
        value.args
    }
}

fn init_command() -> Result<(), OrodruinError> {
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

fn load_config(explicit_path: Option<&Path>) -> Result<LoadedConfig, OrodruinError> {
    let cwd = std::env::current_dir()?;
    Ok(ProjectConfig::load_from(&cwd, explicit_path)?)
}

fn resolve_environment(
    loaded: &LoadedConfig,
    environment: &EnvironmentName,
) -> Result<ResolvedEnvironment, OrodruinError> {
    resolve_environment_by_name(loaded, &environment.env)
}

fn resolve_optional_environment<'a>(
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

fn resolve_default_environment_name<'a>(
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

fn resolve_environment_by_name(
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

fn resolve_run_command(
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

fn resolve_ssh_target(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<String, OrodruinError> {
    let inspect = backend
        .inspect_raw(&resolved.container_name)?
        .ok_or_else(|| {
            OrodruinError::Message(format!(
                "container `{}` is running but inspect returned no payload",
                resolved.container_name
            ))
        })?;

    ssh_host_from_inspect(&inspect).ok_or_else(|| {
        OrodruinError::Message(format!(
            "could not determine an ssh host for `{}` from container inspect output",
            resolved.container_name
        ))
    })
}

fn ssh_host_from_inspect(value: &Value) -> Option<String> {
    if let Some(host) = value
        .get("status")
        .and_then(|status| status.get("networks"))
        .and_then(Value::as_array)
        .and_then(|networks| {
            networks
                .iter()
                .find_map(|network| find_ip_field(network, &["ipv4Address", "ipAddress", "address"]))
        })
    {
        return Some(host);
    }

    find_ip_field(value, &["ipv4Address", "ipAddress", "ip_address", "IPAddress", "ip"])
}

fn find_ip_field(value: &Value, field_names: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for field_name in field_names {
                if let Some(candidate) = map.get(*field_name).and_then(Value::as_str)
                    && let Some(host) = normalize_ip(candidate)
                {
                    return Some(host);
                }
            }

            map.values()
                .find_map(|nested| find_ip_field(nested, field_names))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| find_ip_field(nested, field_names)),
        Value::String(candidate) => normalize_ip(candidate),
        _ => None,
    }
}

fn normalize_ip(candidate: &str) -> Option<String> {
    let host = candidate.split('/').next()?.trim();
    host.parse::<IpAddr>().ok().map(|_| host.to_string())
}

fn build_ssh_spec(resolved: &ResolvedEnvironment, host: &str) -> CommandSpec {
    CommandSpec {
        program: "ssh".into(),
        args: vec![format!("{}@{host}", resolved.user.username)],
    }
}

fn run_host_command(spec: &CommandSpec) -> Result<(), OrodruinError> {
    let status = ProcessCommand::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        return Ok(());
    }

    Err(OrodruinError::CommandFailed {
        command: spec.render(),
        status: status.code(),
    })
}

fn materialize_environment(
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

fn ensure_container_user(
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
            user: None,
        },
    )?;
    Ok(())
}

fn exec_environment(resolved: &ResolvedEnvironment) -> Vec<(String, String)> {
    let mut env = resolved
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    env.push(("HOME".into(), resolved.user.home.clone()));
    env.push(("USER".into(), resolved.user.username.clone()));
    env.push(("LOGNAME".into(), resolved.user.username.clone()));
    env
}

fn bootstrap_user_script() -> &'static str {
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
        echo "uid ${uid} already owned by ${uid_owner}, cannot map ${username}" >&2
        exit 1
    fi

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

mkdir -p "$home"
chown "$uid:$gid" "$home"
"#
}

fn container_exists(
    backend: &dyn ContainerBackend,
    container_name: &str,
) -> Result<bool, OrodruinError> {
    Ok(backend
        .list_all()?
        .iter()
        .any(|summary| summary.matches(container_name)))
}

fn print_summary(name: &str, container_name: &str, summary: Option<&ContainerSummary>) {
    let state = match summary {
        Some(summary) if summary.running => "running",
        Some(summary) => summary.status.as_deref().unwrap_or("created"),
        None => "not-created",
    };
    println!("{name}\t{container_name}\t{state}");
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::VecDeque,
        path::{Path, PathBuf},
        sync::{Mutex, MutexGuard},
    };

    use serde_json::json;

    use super::*;
    use crate::{
        backend::{BackendError, CommandSpec},
        cli::{
            BuilderCommands, Commands, ContainerCommands, EnterCommand, ImageCommands,
            MachineCommands, OptionalPassthroughArgs, RegistryCommands, ResourceCommands,
            SshCommand, SystemCommands,
        },
        config::{EnvironmentConfig, ProjectMetadata},
        env_model::ResolvedBuild,
    };

    #[derive(Default)]
    struct MockBackend {
        list_results: RefCell<VecDeque<Vec<ContainerSummary>>>,
        inspect_value: RefCell<Option<serde_json::Value>>,
        inspect_calls: RefCell<Vec<String>>,
        created: RefCell<Vec<String>>,
        started: RefCell<Vec<String>>,
        deleted: RefCell<Vec<String>>,
        execs: RefCell<Vec<ExecRequest>>,
        exec_results: RefCell<VecDeque<Result<(), BackendError>>>,
        builds: RefCell<Vec<String>>,
        commands: RefCell<Vec<(String, CommandSpec)>>,
    }

    impl MockBackend {
        fn with_lists(lists: Vec<Vec<ContainerSummary>>) -> Self {
            Self {
                list_results: RefCell::new(lists.into()),
                ..Self::default()
            }
        }
    }

    static CURRENT_DIR_LOCK: Mutex<()> = Mutex::new(());

    struct CurrentDirGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        previous: PathBuf,
    }

    impl<'a> CurrentDirGuard<'a> {
        fn enter(path: &Path) -> Self {
            let lock = CURRENT_DIR_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for CurrentDirGuard<'_> {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).unwrap();
        }
    }

    impl ContainerBackend for MockBackend {
        fn list_all(&self) -> Result<Vec<ContainerSummary>, BackendError> {
            Ok(self
                .list_results
                .borrow_mut()
                .pop_front()
                .unwrap_or_default())
        }

        fn inspect_raw(
            &self,
            container_name: &str,
        ) -> Result<Option<serde_json::Value>, BackendError> {
            self.inspect_calls
                .borrow_mut()
                .push(container_name.to_string());
            Ok(self.inspect_value.borrow().clone())
        }

        fn build_image(&self, build: &ResolvedBuild) -> Result<(), BackendError> {
            self.builds.borrow_mut().push(build.tag.clone());
            Ok(())
        }

        fn create(&self, environment: &ResolvedEnvironment) -> Result<(), BackendError> {
            self.created
                .borrow_mut()
                .push(environment.container_name.clone());
            Ok(())
        }

        fn start(&self, container_name: &str) -> Result<(), BackendError> {
            self.started.borrow_mut().push(container_name.to_string());
            Ok(())
        }

        fn exec(&self, _container_name: &str, request: &ExecRequest) -> Result<(), BackendError> {
            self.execs.borrow_mut().push(request.clone());
            self.exec_results.borrow_mut().pop_front().unwrap_or(Ok(()))
        }

        fn delete(&self, container_name: &str) -> Result<(), BackendError> {
            self.deleted.borrow_mut().push(container_name.to_string());
            Ok(())
        }

        fn run_command(&self, step: &str, spec: &CommandSpec) -> Result<(), BackendError> {
            self.commands
                .borrow_mut()
                .push((step.to_string(), spec.clone()));
            Ok(())
        }
    }

    fn write_config(root: &Path) {
        fs::write(
            root.join(CONFIG_FILE_NAME),
            r#"
                [project]
                name = "demo"

                [envs.dev]
                image = "ubuntu:latest"
                shell = ["/bin/bash"]
                startup_command = ["sleep", "infinity"]

                [envs.ci]
                build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
                default_command = ["cargo", "test"]
            "#,
        )
        .unwrap();
    }

    fn resolved_name(root: &Path, env_name: &str) -> String {
        let loaded = ProjectConfig::load_from(root, Some(&root.join(CONFIG_FILE_NAME))).unwrap();
        let config = loaded.config.envs.get(env_name).unwrap();
        ResolvedEnvironment::resolve(&loaded.root, &loaded.config, env_name, config).container_name
    }

    #[test]
    fn create_is_idempotent_for_existing_running_container() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "dev");

        let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
            id: container_name.clone(),
            name: Some(container_name),
            status: Some("running".into()),
            running: true,
        }]]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Create(EnvironmentName { env: "dev".into() }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert!(backend.created.borrow().is_empty());
        assert!(backend.started.borrow().is_empty());
    }

    #[test]
    fn enter_starts_before_exec_when_needed() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "dev");

        let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
            id: container_name.clone(),
            name: Some(container_name),
            status: Some("stopped".into()),
            running: false,
        }]]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.started.borrow().len(), 1);
        assert_eq!(backend.execs.borrow().len(), 2);
        assert_eq!(
            backend.execs.borrow()[1].command,
            vec![String::from("/bin/bash")]
        );
        assert_eq!(
            backend.execs.borrow()[1].user.as_ref().map(|user| user.uid),
            Some(unsafe { libc::getuid() })
        );
    }

    #[test]
    fn enter_ignores_interactive_shell_exit_status() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.exec_results.borrow_mut() = VecDeque::from([
            Ok(()),
            Err(BackendError::CommandFailed {
                step: "exec command".into(),
                command: "container exec demo /bin/bash".into(),
                status: Some(127),
                stderr: String::new(),
            }),
        ]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(
            backend.execs.borrow()[1].command,
            vec![String::from("/bin/bash")]
        );
    }

    #[test]
    fn rm_targets_resolved_container() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "dev");

        let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
            id: container_name.clone(),
            name: Some(container_name),
            status: Some("running".into()),
            running: true,
        }]]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Rm(EnvironmentName { env: "dev".into() }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.deleted.borrow().len(), 1);
    }

    #[test]
    fn run_uses_default_command_when_not_provided() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Run(RunCommand {
                env: "ci".into(),
                command: vec![],
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(
            backend.execs.borrow()[1].command,
            vec![String::from("cargo"), String::from("test")]
        );
        assert_eq!(backend.builds.borrow()[0], "demo-ci:dev");
    }

    #[test]
    fn inspect_reports_backend_payload() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let container_name = resolved_name(tempdir.path(), "dev");
        let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
            id: container_name.clone(),
            name: Some(container_name.clone()),
            status: Some("running".into()),
            running: true,
        }]]);
        *backend.inspect_value.borrow_mut() = Some(json!({ "name": "demo" }));

        let loaded = load_config(None).unwrap();
        let resolved =
            resolve_environment(&loaded, &EnvironmentName { env: "dev".into() }).unwrap();
        let payload = backend
            .inspect_raw(&resolved.container_name)
            .unwrap()
            .unwrap();
        assert_eq!(backend.inspect_calls.borrow().as_slice(), &[container_name]);
        assert_eq!(payload["name"], "demo");
    }

    #[test]
    fn inspect_skips_backend_lookup_for_missing_container() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Inspect(EnvironmentName { env: "dev".into() }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert!(backend.inspect_calls.borrow().is_empty());
    }

    #[test]
    fn existing_built_container_does_not_rebuild_image() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "ci");

        let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
            id: container_name.clone(),
            name: Some(container_name),
            status: Some("running".into()),
            running: true,
        }]]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Create(EnvironmentName { env: "ci".into() }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert!(backend.builds.borrow().is_empty());
    }

    #[test]
    fn project_resolution_uses_config_name() {
        let project = ProjectConfig {
            project: ProjectMetadata {
                name: Some("Demo".into()),
                default_env: None,
            },
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            env: Default::default(),
            preserve_env: vec![],
            mounts: vec![],
            shell: None,
            startup_command: None,
            default_command: None,
        };

        let resolved = ResolvedEnvironment::resolve(Path::new("/tmp/demo"), &project, "dev", &env);
        assert!(resolved.container_name.starts_with("orodruin-demo-"));
    }

    #[test]
    fn enter_bootstraps_user_before_shell_exec() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
            }),
        };

        run_with_backend(cli, &backend).unwrap();

        let execs = backend.execs.borrow();
        assert_eq!(execs[0].command[0], "/bin/sh");
        assert!(execs[0].command[2].contains("useradd -K UID_MIN=0 -K UID_MAX=60000"));
        assert!(execs[0].command[2].contains("useradd -m -d \"$home\" -u \"$uid\" -g \"$gid\" -s /bin/sh \"$username\""));
        assert_eq!(execs[0].user, None);
        assert_eq!(
            execs[1].env.iter().find(|(key, _)| key == "HOME"),
            Some(&(
                String::from("HOME"),
                format!("/home/{}", execs[1].user.as_ref().unwrap().username)
            ))
        );
    }

    #[test]
    fn pull_dispatches_to_image_pull() {
        let backend = MockBackend::default();
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Pull(RequiredPassthroughArgs {
                args: vec!["alpine:latest".into()],
            }),
        };

        run_with_backend(cli, &backend).unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].0, "pull image");
        assert_eq!(commands[0].1.args, ["image", "pull", "alpine:latest"]);
    }

    #[test]
    fn ps_dispatches_to_container_list() {
        let backend = MockBackend::default();
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ps(OptionalPassthroughArgs {
                args: vec!["--all".into()],
            }),
        };

        run_with_backend(cli, &backend).unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].0, "list containers");
        assert_eq!(commands[0].1.args, ["list", "--all"]);
    }

    #[test]
    fn nested_registry_and_resource_commands_map_to_container_cli() {
        let backend = MockBackend::default();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Registry(RegistryCommands::List(OptionalPassthroughArgs {
                    args: vec![],
                })),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Prune(OptionalPassthroughArgs {
                    args: vec![],
                })),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Volume(ResourceCommands::Remove(RequiredPassthroughArgs {
                    args: vec!["cache".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["registry", "list"]);
        assert_eq!(commands[1].1.args, ["image", "prune"]);
        assert_eq!(commands[2].1.args, ["volume", "delete", "cache"]);
    }

    #[test]
    fn nested_image_commands_map_to_container_cli() {
        let backend = MockBackend::default();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Inspect(RequiredPassthroughArgs {
                    args: vec!["alpine:latest".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Push(RequiredPassthroughArgs {
                    args: vec!["demo:latest".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Tag(RequiredPassthroughArgs {
                    args: vec!["demo:latest".into(), "demo:v1".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["image", "inspect", "alpine:latest"]);
        assert_eq!(commands[1].1.args, ["image", "push", "demo:latest"]);
        assert_eq!(commands[2].1.args, ["image", "tag", "demo:latest", "demo:v1"]);
    }

    #[test]
    fn image_load_and_save_map_to_container_cli() {
        let backend = MockBackend::default();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Load(OptionalPassthroughArgs {
                    args: vec!["-i".into(), "image.tar".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Image(ImageCommands::Save(OptionalPassthroughArgs {
                    args: vec!["-o".into(), "image.tar".into(), "demo:latest".into()],
                })),
            },
            &backend,
        )
        .unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["image", "load", "-i", "image.tar"]);
        assert_eq!(commands[1].1.args, ["image", "save", "-o", "image.tar", "demo:latest"]);
    }

    #[test]
    fn nested_container_commands_map_to_container_cli() {
        let backend = MockBackend::default();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Container(ContainerCommands::Inspect(
                    RequiredPassthroughArgs {
                        args: vec!["demo".into()],
                    },
                )),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Container(ContainerCommands::Start(
                    RequiredPassthroughArgs {
                        args: vec!["demo".into()],
                    },
                )),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Container(ContainerCommands::Stop(
                    RequiredPassthroughArgs {
                        args: vec!["demo".into()],
                    },
                )),
            },
            &backend,
        )
        .unwrap();

        run_with_backend(
            Cli {
                debug: false,
                config: None,
                command: Commands::Container(ContainerCommands::Prune(
                    OptionalPassthroughArgs { args: vec![] },
                )),
            },
            &backend,
        )
        .unwrap();

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["inspect", "demo"]);
        assert_eq!(commands[1].1.args, ["start", "demo"]);
        assert_eq!(commands[2].1.args, ["stop", "demo"]);
        assert_eq!(commands[3].1.args, ["prune"]);
    }

    #[test]
    fn extended_container_commands_map_to_container_cli() {
        let backend = MockBackend::default();

        for command in [
            Commands::Container(ContainerCommands::Create(OptionalPassthroughArgs {
                args: vec!["--name".into(), "demo".into(), "alpine".into()],
            })),
            Commands::Container(ContainerCommands::Run(OptionalPassthroughArgs {
                args: vec!["--rm".into(), "alpine".into(), "echo".into(), "hi".into()],
            })),
            Commands::Container(ContainerCommands::Exec(OptionalPassthroughArgs {
                args: vec!["demo".into(), "sh".into()],
            })),
            Commands::Container(ContainerCommands::Kill(OptionalPassthroughArgs {
                args: vec!["demo".into()],
            })),
            Commands::Container(ContainerCommands::Stats(OptionalPassthroughArgs {
                args: vec!["demo".into()],
            })),
            Commands::Container(ContainerCommands::Export(OptionalPassthroughArgs {
                args: vec!["demo".into()],
            })),
        ] {
            run_with_backend(
                Cli {
                    debug: false,
                    config: None,
                    command,
                },
                &backend,
            )
            .unwrap();
        }

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["create", "--name", "demo", "alpine"]);
        assert_eq!(commands[1].1.args, ["run", "--rm", "alpine", "echo", "hi"]);
        assert_eq!(commands[2].1.args, ["exec", "demo", "sh"]);
        assert_eq!(commands[3].1.args, ["kill", "demo"]);
        assert_eq!(commands[4].1.args, ["stats", "demo"]);
        assert_eq!(commands[5].1.args, ["export", "demo"]);
    }

    #[test]
    fn builder_system_and_machine_commands_map_to_container_cli() {
        let backend = MockBackend::default();

        for command in [
            Commands::Builder(BuilderCommands::Status(OptionalPassthroughArgs { args: vec![] })),
            Commands::System(SystemCommands::Version(OptionalPassthroughArgs { args: vec![] })),
            Commands::Machine(MachineCommands::List(OptionalPassthroughArgs { args: vec![] })),
            Commands::Machine(MachineCommands::SetDefault(OptionalPassthroughArgs {
                args: vec!["desktop".into()],
            })),
        ] {
            run_with_backend(
                Cli {
                    debug: false,
                    config: None,
                    command,
                },
                &backend,
            )
            .unwrap();
        }

        let commands = backend.commands.borrow();
        assert_eq!(commands[0].1.args, ["builder", "status"]);
        assert_eq!(commands[1].1.args, ["system", "version"]);
        assert_eq!(commands[2].1.args, ["machine", "list"]);
        assert_eq!(commands[3].1.args, ["machine", "set-default", "desktop"]);
    }

    #[test]
    fn enter_uses_project_default_env_when_omitted() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join(CONFIG_FILE_NAME),
            r#"
                [project]
                name = "demo"
                default_env = "ci"

                [envs.dev]
                image = "ubuntu:latest"

                [envs.ci]
                build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
            "#,
        )
        .unwrap();
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.builds.borrow().as_slice(), ["demo-ci:dev"]);
    }

    #[test]
    fn ssh_print_uses_project_default_env_when_omitted() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join(CONFIG_FILE_NAME),
            r#"
                [project]
                name = "demo"
                default_env = "ci"

                [envs.dev]
                image = "ubuntu:latest"

                [envs.ci]
                build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
            "#,
        )
        .unwrap();
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "ci");

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.inspect_value.borrow_mut() = Some(json!({
            "status": {
                "networks": [{ "ipv4Address": "192.168.64.2/24" }]
            }
        }));
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: None,
                print: true,
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.builds.borrow().as_slice(), ["demo-ci:dev"]);
        assert_eq!(backend.inspect_calls.borrow().as_slice(), [container_name]);
    }

    #[test]
    fn enter_uses_single_environment_when_omitted() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join(CONFIG_FILE_NAME),
            r#"
                [project]
                name = "demo"

                [envs.dev]
                image = "ubuntu:latest"
                shell = ["/bin/bash"]
            "#,
        )
        .unwrap();
        let _guard = CurrentDirGuard::enter(tempdir.path());
        let container_name = resolved_name(tempdir.path(), "dev");

        let backend = MockBackend::with_lists(vec![vec![]]);
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.created.borrow().as_slice(), [container_name]);
    }

    #[test]
    fn enter_without_env_errors_when_ambiguous() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::default();
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(error.to_string().contains("set project.default_env"));
    }

    #[test]
    fn ssh_without_env_errors_when_ambiguous() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::default();
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: None,
                print: true,
            }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(error.to_string().contains("`orodruin ssh` needs an environment name"));
    }

    #[test]
    fn ssh_host_uses_network_ipv4_address_without_cidr() {
        let host = ssh_host_from_inspect(&json!({
            "status": {
                "networks": [{
                    "ipv4Address": "192.168.64.2/24",
                    "ipv6Address": "fdc8:9ac1:53c8:9dd2:f85b:11ff:fe51:30c9/64"
                }]
            }
        }));

        assert_eq!(host.as_deref(), Some("192.168.64.2"));
    }

    #[test]
    fn ssh_print_errors_when_inspect_has_no_ip() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.inspect_value.borrow_mut() = Some(json!({ "status": { "state": "running" } }));
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: Some("dev".into()),
                print: true,
            }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(error.to_string().contains("could not determine an ssh host"));
    }

    #[test]
    fn completions_render_current_subcommands() {
        let output = render_completions(CompletionsCommand {
            shell: clap_complete::Shell::Bash,
        })
        .unwrap();

        assert!(output.contains("completions"));
        assert!(output.contains("orodruin"));
        assert!(output.contains("inspect"));
        assert!(output.contains("ssh"));
    }
}
