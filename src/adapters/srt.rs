//! The sandbox runtime, `srt`, from kelpie's own tools
//!
//! It is the engine under Claude Code's own sandbox: Seatbelt on macOS,
//! bubblewrap on Linux, and a proxy that holds the network to a host list.
//! Kelpie pins its version in [`crate::preview::PACKAGE`]. On macOS a sandbox
//! cannot start inside another, so nothing it runs may start one of its own.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::ports::{Policy, Sandbox, SandboxError};
use crate::preview::Tools;

/// The sandbox runtime installed under kelpie's tools
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRuntime {
    tools: Tools,
}

impl SandboxRuntime {
    /// The runtime `shep kelpie tools install` put in `tools`
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
}

impl Sandbox for SandboxRuntime {
    fn wrap(
        &self,
        policy: &Policy,
        settings: &Path,
        command: &Command,
    ) -> Result<Command, SandboxError> {
        let cli = self.tools.sandbox();
        if !cli.is_file() {
            return Err(SandboxError::Missing(cli));
        }
        let text = serde_json::to_string_pretty(&srt_settings(policy)).expect("settings are JSON");
        let folder = settings.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(folder)
            .and_then(|()| std::fs::write(settings, text))
            .map_err(|e| SandboxError::Settings(settings.to_owned(), e.kind().to_string()))?;
        let mut wrapped = Command::new("node");
        wrapped
            .arg(cli)
            .arg("--settings")
            .arg(settings)
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        if let Some(dir) = command.get_current_dir() {
            wrapped.current_dir(dir);
        }
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => wrapped.env(name, value),
                None => wrapped.env_remove(name),
            };
        }
        Ok(wrapped)
    }
}

/// `srt`'s settings file for `policy`
pub(crate) fn srt_settings(policy: &Policy) -> Value {
    // Without `strictAllowlist`, a host off the list is asked about rather than refused.
    let mut network = json!({
        "allowedDomains": policy.hosts,
        "deniedDomains": [],
        "strictAllowlist": true,
    });
    if let Some(forward) = &policy.forward {
        let mut hosts = policy.hosts.clone();
        hosts.push(forward.host.clone());
        network["allowedDomains"] = json!(hosts);
        network["mitmProxy"] = json!({ "socketPath": forward.socket, "domains": [&forward.host] });
    }
    if policy.listen {
        network["allowLocalBinding"] = true.into();
    }
    if !policy.services.is_empty() {
        network["allowMachLookup"] = json!(policy.services);
    }
    if !policy.sockets.is_empty() {
        network["allowUnixSockets"] = json!(policy.sockets);
    }
    json!({
        "network": network,
        "filesystem": {
            "denyRead": policy.no_read,
            "allowRead": policy.read,
            "allowWrite": policy.write,
            "denyWrite": policy.no_write,
        },
        "enableWeakerNetworkIsolation": policy.verify_tls,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_missing_runtime_refuses_to_run_anything() {
        let dir = tempfile::tempdir().unwrap();
        let srt = SandboxRuntime::new(Tools::at(dir.path().to_owned()));
        let settings = dir.path().join("call.srt.json");
        let err = srt
            .wrap(&Policy::default(), &settings, &Command::new("claude"))
            .unwrap_err();
        assert!(matches!(err, SandboxError::Missing(_)), "{err:?}");
        assert!(
            err.to_string().contains("shep kelpie tools install"),
            "{err}"
        );
        assert!(!settings.exists());
    }

    #[test]
    fn a_forwarded_host_is_the_only_one_allowed_and_its_traffic_goes_to_the_socket() {
        let policy = Policy {
            forward: Some(crate::ports::Forward {
                host: "model.kelpie.test".into(),
                socket: PathBuf::from("/k/worker/model.sock"),
            }),
            ..Policy::default()
        };
        let network = &srt_settings(&policy)["network"];
        assert_eq!(network["allowedDomains"], json!(["model.kelpie.test"]));
        assert_eq!(
            network["mitmProxy"],
            json!({ "socketPath": "/k/worker/model.sock", "domains": ["model.kelpie.test"] })
        );
        assert!(network.get("allowUnixSockets").is_none());
        let plain = &srt_settings(&Policy::default())["network"];
        assert!(plain.get("mitmProxy").is_none());
    }

    #[test]
    fn the_command_runs_under_srt_with_its_folder_variables_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools::at(dir.path().to_owned());
        std::fs::create_dir_all(tools.sandbox().parent().unwrap()).unwrap();
        std::fs::write(tools.sandbox(), "").unwrap();
        let mut inner = Command::new("claude");
        inner
            .args(["-p", "it's"])
            .current_dir("/k/wt/7")
            .env("CARGO_TARGET_DIR", "/k/targets/7")
            .env_remove("CLAUDECODE");
        let settings = dir.path().join("worker").join("call.srt.json");
        let policy = Policy {
            write: vec![PathBuf::from("/k/wt/7")],
            hosts: vec!["api.anthropic.com".into()],
            ..Policy::default()
        };
        let wrapped = SandboxRuntime::new(tools.clone())
            .wrap(&policy, &settings, &inner)
            .unwrap();
        assert_eq!(wrapped.get_program(), "node");
        let args: Vec<_> = wrapped.get_args().collect();
        let cli = tools.sandbox();
        assert_eq!(
            args,
            [
                cli.as_os_str(),
                "--settings".as_ref(),
                settings.as_os_str(),
                "--".as_ref(),
                "claude".as_ref(),
                "-p".as_ref(),
                "it's".as_ref(),
            ]
        );
        assert_eq!(wrapped.get_current_dir(), Some(Path::new("/k/wt/7")));
        let envs: Vec<_> = wrapped.get_envs().collect();
        assert!(envs.contains(&("CARGO_TARGET_DIR".as_ref(), Some("/k/targets/7".as_ref()))));
        assert!(envs.contains(&("CLAUDECODE".as_ref(), None)));
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(written, srt_settings(&policy));
        assert_eq!(written["network"]["strictAllowlist"], true);
        assert_eq!(written["filesystem"]["allowWrite"], json!(["/k/wt/7"]));
    }
}
