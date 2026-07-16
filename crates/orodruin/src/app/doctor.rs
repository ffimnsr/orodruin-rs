use std::{env, path::Path};

use serde::Serialize;

use crate::{
    backend::{ContainerBackend, ContainerRuntime},
    cli::DoctorCommand,
    config::{EnvironmentConfig, ProjectConfig},
    env_model::ResolvedEnvironment,
    error::OrodruinError,
};

#[derive(Debug, Serialize)]
pub(crate) struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DoctorEnvironmentReport {
    pub env: String,
    pub container: String,
    pub image: String,
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DoctorReport {
    pub healthy: bool,
    pub runtime: String,
    pub runtime_program: String,
    pub config_path: Option<String>,
    pub project_root: Option<String>,
    pub checks: Vec<DoctorCheck>,
    pub environments: Vec<DoctorEnvironmentReport>,
}

pub(crate) fn doctor_check(
    name: impl Into<String>,
    ok: bool,
    detail: impl Into<String>,
) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok,
        detail: detail.into(),
    }
}

pub(crate) fn doctor_environment_report(
    project_root: &Path,
    project: &ProjectConfig,
    env_name: &str,
    config: &EnvironmentConfig,
) -> DoctorEnvironmentReport {
    let resolved = ResolvedEnvironment::resolve(project_root, project, env_name, config);
    let mut checks = vec![doctor_check(
        "project root mount",
        resolved.project_root.exists(),
        format!("host path {}", resolved.project_root.display()),
    )];

    if let Some(build) = &resolved.build {
        checks.push(doctor_check(
            "build context",
            build.context.exists(),
            format!("{}", build.context.display()),
        ));
        if let Some(file) = &build.file {
            checks.push(doctor_check(
                "build file",
                file.exists(),
                format!("{}", file.display()),
            ));
        }
    }

    for mount in &resolved.mounts[1..] {
        checks.push(doctor_check(
            format!("mount {}", mount.target),
            mount.source.exists(),
            format!("host path {}", mount.source.display()),
        ));
    }

    for key in &config.preserve_env {
        let present = env::var_os(key).is_some();
        checks.push(doctor_check(
            format!("preserve env {key}"),
            present,
            if present {
                String::from("available in host environment")
            } else {
                String::from("missing from host environment")
            },
        ));
    }

    let healthy = checks.iter().all(|check| check.ok);
    DoctorEnvironmentReport {
        env: env_name.to_string(),
        container: resolved.container_name,
        image: resolved.image,
        healthy,
        checks,
    }
}

pub(crate) fn runtime_name(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::AppleContainer => "apple-container",
        ContainerRuntime::Podman => "podman",
    }
}

pub(crate) fn runtime_program_display(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::AppleContainer => "Apple Container CLI",
        ContainerRuntime::Podman => "Podman",
    }
}

pub(crate) fn runtime_program_available(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|directory| executable_exists(&directory.join(program)))
}

pub(crate) fn executable_exists(path: &Path) -> bool {
    path.is_file()
}

fn print_json<T: Serialize>(value: &T) -> Result<(), OrodruinError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| OrodruinError::Message(error.to_string()))?
    );
    Ok(())
}

pub(crate) fn print_doctor_report(report: &DoctorReport) {
    println!(
        "doctor: {}",
        if report.healthy {
            "ok"
        } else {
            "problems found"
        }
    );
    println!("runtime: {} ({})", report.runtime, report.runtime_program);
    if let Some(config_path) = &report.config_path {
        println!("config: {config_path}");
    }
    if let Some(project_root) = &report.project_root {
        println!("project root: {project_root}");
    }

    for check in &report.checks {
        println!(
            "check {}: {} ({})",
            check.name,
            if check.ok { "ok" } else { "fail" },
            check.detail
        );
    }

    for environment in &report.environments {
        println!(
            "env {}: {} [{}]",
            environment.env,
            if environment.healthy { "ok" } else { "fail" },
            environment.container
        );
        println!("  image: {}", environment.image);
        for check in &environment.checks {
            println!(
                "  check {}: {} ({})",
                check.name,
                if check.ok { "ok" } else { "fail" },
                check.detail
            );
        }
    }
}

pub(crate) fn doctor_command(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    explicit_path: Option<&Path>,
    command: DoctorCommand,
) -> Result<(), OrodruinError> {
    let report = doctor_report(backend, runtime, explicit_path)?;
    if command.json {
        print_json(&report)?;
    } else {
        print_doctor_report(&report);
    }
    if report.healthy {
        Ok(())
    } else {
        Err(OrodruinError::Message("doctor found problems".into()))
    }
}

pub(crate) fn doctor_report(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    explicit_path: Option<&Path>,
) -> Result<DoctorReport, OrodruinError> {
    let cwd = std::env::current_dir()?;
    let runtime_program = runtime.program().to_string();
    let mut checks = vec![doctor_check(
        "runtime binary",
        runtime_program_available(&runtime_program),
        format!("{} on PATH", runtime_program_display(runtime)),
    )];
    let config_path = match ProjectConfig::locate_path(&cwd, explicit_path) {
        Ok(path) => path,
        Err(error) => {
            checks.push(doctor_check("config file", false, error.to_string()));
            return Ok(DoctorReport {
                healthy: false,
                runtime: runtime_name(runtime).into(),
                runtime_program,
                config_path: None,
                project_root: None,
                checks,
                environments: vec![],
            });
        }
    };

    let loaded = match ProjectConfig::load_path(&config_path) {
        Ok(loaded) => {
            checks.push(doctor_check(
                "config file",
                true,
                format!("loaded {}", config_path.display()),
            ));
            loaded
        }
        Err(error) => {
            checks.push(doctor_check("config file", false, error.to_string()));
            return Ok(DoctorReport {
                healthy: false,
                runtime: runtime_name(runtime).into(),
                runtime_program,
                config_path: Some(config_path.display().to_string()),
                project_root: config_path.parent().map(|path| path.display().to_string()),
                checks,
                environments: vec![],
            });
        }
    };

    let runtime_available = checks.first().map(|check| check.ok).unwrap_or(false);
    if runtime.manages_system_lifecycle() {
        let system_check = if runtime_available {
            match backend.system_running() {
                Ok(true) => doctor_check("container system", true, "running"),
                Ok(false) => doctor_check(
                    "container system",
                    false,
                    "not running; start with `container system start` or rerun commands with `--yes`",
                ),
                Err(error) => doctor_check("container system", false, error.to_string()),
            }
        } else {
            doctor_check(
                "container system",
                false,
                "runtime binary missing; cannot check container system state",
            )
        };
        checks.push(system_check);
    }

    let environments = loaded
        .config
        .envs
        .iter()
        .map(|(name, config)| doctor_environment_report(&loaded.root, &loaded.config, name, config))
        .collect::<Vec<_>>();

    let healthy = checks.iter().all(|check| check.ok)
        && environments.iter().all(|environment| environment.healthy);

    Ok(DoctorReport {
        healthy,
        runtime: runtime_name(runtime).into(),
        runtime_program,
        config_path: Some(loaded.path.display().to_string()),
        project_root: Some(loaded.root.display().to_string()),
        checks,
        environments,
    })
}
