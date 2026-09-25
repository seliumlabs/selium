//! Pure mapping seams: parsed argv → [`ControlRequest`], and
//! [`ControlResponse`] → rendered line + exit code.
//!
//! These functions deliberately stay free of the QUIC connection and the
//! Tokio runtime so the command-mapping behaviour is unit-testable natively.

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use selium_client::{
    ClientIdentity, ConnectOptions,
    selium_service::{ControlRequest, ControlResponse},
};

use crate::cli::{Cli, Command};

/// Builds [`ConnectOptions`] from the connection flags: the bare bridge
/// server name (the single per-platform bridge-server's root wire name), the
/// trusted server root, and the client identity. The requestor's tenant is
/// established by the presented client certificate, not by the name dialled.
pub fn build_connect_options(cli: &Cli) -> Result<ConnectOptions> {
    let ca_pem = std::fs::read(&cli.ca).with_context(|| {
        format!(
            "failed to read server root certificate {}",
            cli.ca.display()
        )
    })?;
    let server_root = selium_client::certificates_from_pem(&ca_pem)?;

    let cert_pem = std::fs::read(&cli.client_cert).with_context(|| {
        format!(
            "failed to read client certificate {}",
            cli.client_cert.display()
        )
    })?;
    let key_pem = std::fs::read(&cli.client_key)
        .with_context(|| format!("failed to read client key {}", cli.client_key.display()))?;
    let identity = Some(ClientIdentity {
        cert_chain: selium_client::certificates_from_pem(&cert_pem)?,
        key: selium_client::private_key_from_pem(&key_pem)?,
    });

    Ok(ConnectOptions {
        server_name: cli.server_name(),
        server_root,
        identity,
        transport: None,
    })
}

/// Builds the typed control request for a parsed command.
///
/// `upload` reads its module bytes from `--file`; every other verb maps its
/// fields directly onto the corresponding [`ControlRequest`] variant.
pub fn build_request(command: &Command) -> Result<ControlRequest> {
    match command {
        Command::Deploy {
            workload,
            replicas,
            module,
        } => Ok(ControlRequest::Deploy {
            workload_id: workload.clone(),
            replicas: *replicas,
            module: module.clone(),
        }),
        Command::Scale { workload, replicas } => Ok(ControlRequest::Scale {
            workload_id: workload.clone(),
            replicas: *replicas,
        }),
        Command::Stop { workload } => Ok(ControlRequest::Stop {
            workload_id: workload.clone(),
        }),
        Command::Status { workload } => Ok(ControlRequest::Status {
            workload_id: workload.clone(),
        }),
        Command::Resolve { uri } => Ok(ControlRequest::Resolve { uri: uri.clone() }),
        Command::Upload { manifest, file } => {
            let bytes = std::fs::read(file)
                .with_context(|| format!("failed to read module file {}", file.display()))?;
            Ok(ControlRequest::Upload {
                manifest: manifest.clone(),
                bytes,
            })
        }
    }
}

/// Renders a control response into a human-readable line and an exit code.
///
/// The parsed command contextualises the success line: `deploy`, `scale`,
/// and `stop` each report their own outcome. Failures depend only on the
/// response: a deferred delegation, a typed control error, and a
/// `status`/`resolve` miss all render a message and a non-zero exit code.
pub fn render(command: &Command, response: ControlResponse) -> (String, ExitCode) {
    match response {
        ControlResponse::Accepted {
            workload_id,
            replicas,
            module,
            delegated,
        } if delegated.applied => {
            let line = match command {
                Command::Deploy { .. } => format!(
                    "workload {workload_id} accepted with {replicas} replicas and module {module}"
                ),
                Command::Scale { .. } => {
                    format!("workload {workload_id} scaled to {replicas} replicas")
                }
                Command::Stop { .. } => format!("workload {workload_id} stopped"),
                // `Accepted` is the reply to deploy/scale/stop; any other
                // verb receiving it is a protocol mismatch, rendered with
                // the generic accepted-outcome line rather than a wrong
                // verb-specific one.
                _ => format!(
                    "workload {workload_id} accepted with {replicas} replicas and module {module}"
                ),
            };
            (line, ExitCode::SUCCESS)
        }
        ControlResponse::Accepted { delegated, .. } => (delegated.context, ExitCode::FAILURE),
        ControlResponse::Uploaded { manifest } => (
            format!("module stored under manifest {manifest}"),
            ExitCode::SUCCESS,
        ),
        ControlResponse::Status {
            deployment: Some(deployment),
        } => (
            format!(
                "workload {}: {} replicas, module {}",
                deployment.workload_id, deployment.replicas, deployment.module,
            ),
            ExitCode::SUCCESS,
        ),
        ControlResponse::Status { deployment: None } => {
            ("not found".to_string(), ExitCode::FAILURE)
        }
        ControlResponse::Resolved {
            target: Some(target),
        } => (
            format!(
                "resolved {}: host {} resource {}",
                target.uri, target.host_id, target.resource_id,
            ),
            ExitCode::SUCCESS,
        ),
        ControlResponse::Resolved { target: None } => ("not found".to_string(), ExitCode::FAILURE),
        ControlResponse::Error { step, context } => {
            (format!("{step}: {context}"), ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use selium_client::selium_service::{
        ControlRequest, ControlResponse, DelegationStatus, Deployment, ResolvedTarget,
    };

    use super::*;

    fn temp_path(stem: &str) -> PathBuf {
        std::env::temp_dir().join(format!("selium-cli-test-{stem}-{}", std::process::id()))
    }

    #[test]
    fn maps_each_verb_to_its_request() {
        assert_eq!(
            build_request(&Command::Deploy {
                workload: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            })
            .unwrap(),
            ControlRequest::Deploy {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            },
        );
        assert_eq!(
            build_request(&Command::Scale {
                workload: "api".to_string(),
                replicas: 5,
            })
            .unwrap(),
            ControlRequest::Scale {
                workload_id: "api".to_string(),
                replicas: 5,
            },
        );
        assert_eq!(
            build_request(&Command::Stop {
                workload: "api".to_string(),
            })
            .unwrap(),
            ControlRequest::Stop {
                workload_id: "api".to_string(),
            },
        );
        assert_eq!(
            build_request(&Command::Status {
                workload: "api".to_string(),
            })
            .unwrap(),
            ControlRequest::Status {
                workload_id: "api".to_string(),
            },
        );
        assert_eq!(
            build_request(&Command::Resolve {
                uri: "sel://acme/lobby".to_string(),
            })
            .unwrap(),
            ControlRequest::Resolve {
                uri: "sel://acme/lobby".to_string(),
            },
        );
    }

    #[test]
    fn upload_reads_module_bytes() {
        let file = temp_path("upload");
        std::fs::write(&file, b"\0asm\x01module").unwrap();

        let request = build_request(&Command::Upload {
            manifest: "api/v1".to_string(),
            file: file.clone(),
        })
        .unwrap();

        assert_eq!(
            request,
            ControlRequest::Upload {
                manifest: "api/v1".to_string(),
                bytes: b"\0asm\x01module".to_vec(),
            },
        );

        std::fs::remove_file(file).unwrap();
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "unwrap_used lint conflicts with clippy's suggested fix"
    )]
    fn upload_missing_file_errors_without_request() {
        let result = build_request(&Command::Upload {
            manifest: "api/v1".to_string(),
            file: temp_path("missing"),
        });
        assert!(result.is_err());
    }

    fn accepted(applied: bool, context: &str) -> ControlResponse {
        ControlResponse::Accepted {
            workload_id: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
            delegated: DelegationStatus {
                step: "scheduler".to_string(),
                applied,
                context: context.to_string(),
            },
        }
    }

    #[test]
    fn renders_deploy_accepted_as_success() {
        let command = Command::Deploy {
            workload: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
        };
        let (line, code) = render(&command, accepted(true, "placed"));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(
            line,
            "workload api accepted with 3 replicas and module api/v1"
        );
    }

    #[test]
    fn renders_scale_accepted_as_scaled() {
        let command = Command::Scale {
            workload: "api".to_string(),
            replicas: 5,
        };
        let (line, code) = render(
            &command,
            ControlResponse::Accepted {
                workload_id: "api".to_string(),
                replicas: 5,
                module: "api/v1".to_string(),
                delegated: DelegationStatus {
                    step: "scheduler".to_string(),
                    applied: true,
                    context: "placed".to_string(),
                },
            },
        );
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, "workload api scaled to 5 replicas");
    }

    #[test]
    fn renders_stop_accepted_as_stopped() {
        let command = Command::Stop {
            workload: "api".to_string(),
        };
        let (line, code) = render(&command, accepted(true, "placed"));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, "workload api stopped");
    }

    #[test]
    fn renders_deferred_delegation_as_failure() {
        let command = Command::Deploy {
            workload: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
        };
        let (line, code) = render(
            &command,
            accepted(false, "scheduler service not yet online"),
        );
        assert_eq!(code, std::process::ExitCode::FAILURE);
        assert_eq!(line, "scheduler service not yet online");
    }

    #[test]
    fn renders_deferred_scale_as_failure() {
        let command = Command::Scale {
            workload: "api".to_string(),
            replicas: 5,
        };
        let (_, code) = render(
            &command,
            accepted(false, "scheduler service not yet online"),
        );
        assert_eq!(code, std::process::ExitCode::FAILURE);
    }

    #[test]
    fn renders_typed_error_with_step_and_context() {
        let command = Command::Deploy {
            workload: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
        };
        let (line, code) = render(
            &command,
            ControlResponse::Error {
                step: "storage".to_string(),
                context: "blob missing".to_string(),
            },
        );
        assert_eq!(code, std::process::ExitCode::FAILURE);
        assert_eq!(line, "storage: blob missing");
    }

    #[test]
    fn renders_uploaded_as_success() {
        let command = Command::Upload {
            manifest: "api/v1".to_string(),
            file: "unused".into(),
        };
        let (line, code) = render(
            &command,
            ControlResponse::Uploaded {
                manifest: "api/v1".to_string(),
            },
        );
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, "module stored under manifest api/v1");
    }

    #[test]
    fn renders_status_recorded_and_not_found() {
        let command = Command::Status {
            workload: "api".to_string(),
        };
        let (line, code) = render(
            &command,
            ControlResponse::Status {
                deployment: Some(Deployment {
                    workload_id: "api".to_string(),
                    replicas: 3,
                    module: "api/v1".to_string(),
                }),
            },
        );
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, "workload api: 3 replicas, module api/v1");

        let (line, code) = render(&command, ControlResponse::Status { deployment: None });
        assert_eq!(code, std::process::ExitCode::FAILURE);
        assert_eq!(line, "not found");
    }

    #[test]
    fn renders_resolved_target_and_not_found() {
        let command = Command::Resolve {
            uri: "sel://acme/lobby".to_string(),
        };
        let (line, code) = render(
            &command,
            ControlResponse::Resolved {
                target: Some(ResolvedTarget {
                    uri: "sel://acme/lobby".to_string(),
                    host_id: "host-1".to_string(),
                    resource_id: 42,
                }),
            },
        );
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, "resolved sel://acme/lobby: host host-1 resource 42");

        let (line, code) = render(&command, ControlResponse::Resolved { target: None });
        assert_eq!(code, std::process::ExitCode::FAILURE);
        assert_eq!(line, "not found");
    }

    #[test]
    fn builds_connect_options_from_fixture_certificates() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../guests/connector-quic/tests/fixtures"
        );
        let cli = Cli {
            connector: "127.0.0.1:4433".to_string(),
            ca: PathBuf::from(format!("{dir}/bridge_cert.pem")),
            client_cert: PathBuf::from(format!("{dir}/client_cert.pem")),
            client_key: PathBuf::from(format!("{dir}/client_key.pem")),
            command: Command::Status {
                workload: "api".to_string(),
            },
        };

        let options = build_connect_options(&cli).unwrap();

        // The dialled name is the bare root wire name: the tenant comes from
        // the presented leaf, not the name dialled.
        assert_eq!(options.server_name, "bridge");
        // The control route is the single control plane's root route.
        assert_eq!(cli.control_route(), "sel:///control");
        assert!(!options.server_root.is_empty());
        assert!(
            options
                .identity
                .is_some_and(|identity| !identity.cert_chain.is_empty())
        );
    }
}
