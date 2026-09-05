use serde_json::Value;
use thiserror::Error;

use std::io;
use std::process::Command;

use super::identity::{parse_stored_tailscale_login, LoginParseError};
use super::protocol::ServeSpec;

pub const MINIMUM_TAILSCALE_VERSION: &str = "1.52.0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, arguments: &[&str]) -> io::Result<CommandOutput>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, arguments: &[&str]) -> io::Result<CommandOutput> {
        let output = Command::new(program).args(arguments).output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscalePeer {
    pub dns_name: String,
    pub online: bool,
    pub tagged: bool,
}

impl TailscalePeer {
    pub fn audetic_base_url(&self) -> String {
        ServeSpec::audetic().base_url(&self.dns_name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleStatus {
    pub version: semver::Version,
    pub backend_state: String,
    pub self_dns_name: String,
    pub owner_login: String,
    pub self_is_tagged: bool,
    pub peers: Vec<TailscalePeer>,
}

impl TailscaleStatus {
    pub fn discoverable_peers(&self) -> impl Iterator<Item = &TailscalePeer> {
        self.peers.iter().filter(|peer| peer.online && !peer.tagged)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingState {
    Vacant,
    OwnedByAudetic,
    Collision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeAssessment {
    pub mapping: MappingState,
    pub funnel_enabled: bool,
}

#[derive(Debug, Error)]
pub enum TailscaleError {
    #[error("could not execute tailscale: {0}")]
    Execute(#[source] io::Error),
    #[error("tailscale {command} failed with status {status}: {stderr}")]
    CommandFailed {
        command: String,
        status: i32,
        stderr: String,
    },
    #[error("tailscale returned invalid JSON for {command}: {source}")]
    InvalidJson {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("tailscale status is missing {0}")]
    MissingStatusField(&'static str),
    #[error("tailscale {actual} is too old; Audetic requires at least {minimum}")]
    UnsupportedVersion {
        actual: semver::Version,
        minimum: semver::Version,
    },
    #[error("the local Tailscale device is tagged; identity headers require an untagged device")]
    TaggedDevice,
    #[error("invalid Tailscale owner login: {0}")]
    InvalidOwnerLogin(#[from] LoginParseError),
    #[error("the Audetic Tailscale Serve port already has a non-Audetic Serve mapping")]
    ServeCollision,
    #[error("Tailscale Funnel is enabled on the Audetic Serve port")]
    FunnelEnabled,
}

pub struct Tailscale<R> {
    runner: R,
}

impl<R: CommandRunner> Tailscale<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        let value = self.run_json(&["status", "--json"])?;
        parse_status(&value)
    }

    pub fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
        let spec = ServeSpec::audetic();
        let serve = self.run_json(&["serve", "status", "--json"])?;
        let funnel = self.run_json(&["funnel", "status", "--json"])?;
        Ok(ServeAssessment {
            mapping: mapping_state(&serve, spec),
            funnel_enabled: funnel_enabled_on_port(&funnel, spec.https_port()),
        })
    }

    /// Apply the exact Audetic mapping, returning whether this call created it.
    pub fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let assessment = self.serve_assessment()?;
        match assessment.mapping {
            MappingState::Collision => return Err(TailscaleError::ServeCollision),
            MappingState::OwnedByAudetic if !assessment.funnel_enabled => return Ok(false),
            MappingState::Vacant | MappingState::OwnedByAudetic => {}
        }
        if assessment.funnel_enabled {
            return Err(TailscaleError::FunnelEnabled);
        }

        self.run_checked_owned(&ServeSpec::audetic().apply_arguments())?;
        Ok(true)
    }

    pub fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let assessment = self.serve_assessment()?;
        if assessment.mapping != MappingState::OwnedByAudetic {
            return Ok(false);
        }
        self.run_checked_owned(&ServeSpec::audetic().remove_arguments())?;
        Ok(true)
    }

    pub fn serve_preview(&self) -> String {
        format!(
            "tailscale {}",
            ServeSpec::audetic().apply_arguments().join(" ")
        )
    }

    fn run_json(&self, arguments: &[&str]) -> Result<Value, TailscaleError> {
        let command = arguments.join(" ");
        let bytes = self.run_checked(arguments)?;
        serde_json::from_slice(&bytes)
            .map_err(|source| TailscaleError::InvalidJson { command, source })
    }

    fn run_checked(&self, arguments: &[&str]) -> Result<Vec<u8>, TailscaleError> {
        let output = self
            .runner
            .run("tailscale", arguments)
            .map_err(TailscaleError::Execute)?;
        if output.status != 0 {
            return Err(TailscaleError::CommandFailed {
                command: arguments.join(" "),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(output.stdout)
    }

    fn run_checked_owned(&self, arguments: &[String]) -> Result<Vec<u8>, TailscaleError> {
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        self.run_checked(&arguments)
    }
}

/// Narrow command boundary owned by `ServeManager` and replaced by fakes in tests.
pub trait TailscaleControl: Send + Sync {
    fn status(&self) -> Result<TailscaleStatus, TailscaleError>;
    fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError>;
    fn apply_audetic_serve(&self) -> Result<bool, TailscaleError>;
    fn remove_audetic_serve(&self) -> Result<bool, TailscaleError>;
    fn serve_preview(&self) -> String;
}

impl<R: CommandRunner> TailscaleControl for Tailscale<R> {
    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        Tailscale::status(self)
    }

    fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
        Tailscale::serve_assessment(self)
    }

    fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
        Tailscale::apply_audetic_serve(self)
    }

    fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
        Tailscale::remove_audetic_serve(self)
    }

    fn serve_preview(&self) -> String {
        Tailscale::serve_preview(self)
    }
}

/// Exact, path-scoped recovery command printed when uninstall cannot execute
/// Tailscale. This deliberately never suggests `tailscale serve reset`, which
/// would destroy mappings owned by other applications.
pub(crate) fn audetic_serve_cleanup_command() -> String {
    format!(
        "tailscale {}",
        ServeSpec::audetic().remove_arguments().join(" ")
    )
}

fn parse_status(value: &Value) -> Result<TailscaleStatus, TailscaleError> {
    let version_text = string_field(value, "Version")?;
    let version = semver::Version::parse(version_text)
        .map_err(|_| TailscaleError::MissingStatusField("Version"))?;
    let minimum = semver::Version::parse(MINIMUM_TAILSCALE_VERSION).expect("valid constant");
    if version < minimum {
        return Err(TailscaleError::UnsupportedVersion {
            actual: version,
            minimum,
        });
    }

    let own = value
        .get("Self")
        .and_then(Value::as_object)
        .ok_or(TailscaleError::MissingStatusField("Self"))?;
    let self_is_tagged = has_tags(own.get("Tags"));
    if self_is_tagged {
        return Err(TailscaleError::TaggedDevice);
    }
    let user_id = own
        .get("UserID")
        .and_then(value_as_map_key)
        .ok_or(TailscaleError::MissingStatusField("Self.UserID"))?;
    let owner_raw = value
        .get("User")
        .and_then(Value::as_object)
        .and_then(|users| users.get(&user_id))
        .and_then(|user| user.get("LoginName"))
        .and_then(Value::as_str)
        .ok_or(TailscaleError::MissingStatusField("User.LoginName"))?;

    let peers = value
        .get("Peer")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|peers| peers.values())
        .filter_map(|peer| {
            Some(TailscalePeer {
                dns_name: peer.get("DNSName")?.as_str()?.to_owned(),
                online: peer.get("Online").and_then(Value::as_bool).unwrap_or(false),
                tagged: has_tags(peer.get("Tags")),
            })
        })
        .collect();

    Ok(TailscaleStatus {
        version,
        backend_state: string_field(value, "BackendState")?.to_owned(),
        self_dns_name: own
            .get("DNSName")
            .and_then(Value::as_str)
            .ok_or(TailscaleError::MissingStatusField("Self.DNSName"))?
            .to_owned(),
        owner_login: parse_stored_tailscale_login(owner_raw)?,
        self_is_tagged,
        peers,
    })
}

fn string_field<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, TailscaleError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(TailscaleError::MissingStatusField(field))
}

fn value_as_map_key(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn has_tags(tags: Option<&Value>) -> bool {
    tags.and_then(Value::as_array)
        .is_some_and(|tags| !tags.is_empty())
}

fn mapping_state(config: &Value, spec: ServeSpec) -> MappingState {
    let Some(web) = config.get("Web").and_then(Value::as_object) else {
        return if port_has_mapping(config, spec.https_port()) {
            MappingState::Collision
        } else {
            MappingState::Vacant
        };
    };

    let on_port: Vec<_> = web
        .iter()
        .filter(|(endpoint, _)| endpoint_uses_port(endpoint, spec.https_port()))
        .collect();
    if on_port.is_empty() {
        return if tcp_has_port(config, spec.https_port()) {
            MappingState::Collision
        } else {
            MappingState::Vacant
        };
    }

    if on_port.len() == 1 && is_exact_audetic_mapping(on_port[0].1, spec) {
        MappingState::OwnedByAudetic
    } else {
        MappingState::Collision
    }
}

fn is_exact_audetic_mapping(server: &Value, spec: ServeSpec) -> bool {
    let Some(handlers) = server.get("Handlers").and_then(Value::as_object) else {
        return false;
    };
    if handlers.len() != 1 {
        return false;
    }
    handlers
        .get(spec.mount_path())
        .or_else(|| handlers.get(&format!("{}/", spec.mount_path())))
        .and_then(|handler| handler.get("Proxy"))
        .and_then(Value::as_str)
        == Some(spec.proxy_url())
}

fn port_has_mapping(config: &Value, port: u16) -> bool {
    tcp_has_port(config, port)
        || config
            .get("Web")
            .and_then(Value::as_object)
            .is_some_and(|web| {
                web.keys()
                    .any(|endpoint| endpoint_uses_port(endpoint, port))
            })
}

fn funnel_enabled_on_port(config: &Value, port: u16) -> bool {
    port_has_mapping(config, port)
        || config
            .get("AllowFunnel")
            .and_then(Value::as_object)
            .is_some_and(|endpoints| {
                endpoints.iter().any(|(endpoint, enabled)| {
                    endpoint_uses_port(endpoint, port) && enabled.as_bool().unwrap_or(false)
                })
            })
}

fn tcp_has_port(config: &Value, port: u16) -> bool {
    config
        .get("TCP")
        .and_then(Value::as_object)
        .is_some_and(|tcp| tcp.contains_key(&port.to_string()))
}

fn endpoint_uses_port(endpoint: &str, port: u16) -> bool {
    endpoint
        .rsplit_once(':')
        .and_then(|(_, endpoint_port)| endpoint_port.parse::<u16>().ok())
        == Some(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        outputs: Mutex<VecDeque<CommandOutput>>,
    }

    impl FakeRunner {
        fn with_json(outputs: &[&str]) -> Self {
            Self {
                calls: Mutex::default(),
                outputs: Mutex::new(
                    outputs
                        .iter()
                        .map(|json| CommandOutput {
                            status: 0,
                            stdout: json.as_bytes().to_vec(),
                            stderr: Vec::new(),
                        })
                        .collect(),
                ),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
            Self {
                calls: Mutex::default(),
                outputs: Mutex::new(outputs.into()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, arguments: &[&str]) -> io::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(arguments.iter().map(|argument| (*argument).to_owned()));
            self.calls.lock().unwrap().push(call);
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no fake output"))
        }
    }

    fn status_json(version: &str, tags: &str) -> String {
        format!(
            r#"{{
                "Version":"{version}",
                "BackendState":"Running",
                "Self":{{"DNSName":"hub.example.ts.net.","UserID":42,"Tags":{tags}}},
                "User":{{"42":{{"LoginName":"=?utf-8?q?m=C3=A1t@example.com?="}}}},
                "Peer":{{
                    "a":{{"DNSName":"online.example.ts.net.","Online":true}},
                    "b":{{"DNSName":"offline.example.ts.net.","Online":false}},
                    "c":{{"DNSName":"tagged.example.ts.net.","Online":true,"Tags":["tag:server"]}}
                }}
            }}"#
        )
    }

    #[test]
    fn status_uses_exact_command_and_exposes_only_online_untagged_discovery_peers() {
        let runner = FakeRunner::with_json(&[&status_json("1.52.0", "[]")]);
        let tailscale = Tailscale::new(runner);

        let status = tailscale.status().unwrap();

        assert_eq!(status.owner_login, "mát@example.com");
        assert_eq!(
            status
                .discoverable_peers()
                .map(TailscalePeer::audetic_base_url)
                .collect::<Vec<_>>(),
            vec!["https://online.example.ts.net:8443/audetic/"]
        );
        assert_eq!(
            tailscale.runner.calls(),
            vec![vec!["tailscale", "status", "--json"]]
        );
    }

    #[test]
    fn status_rejects_old_tailscale_and_tagged_local_devices() {
        let old = Tailscale::new(FakeRunner::with_json(&[&status_json("1.51.9", "[]")]));
        assert!(matches!(
            old.status(),
            Err(TailscaleError::UnsupportedVersion { .. })
        ));

        let tagged = Tailscale::new(FakeRunner::with_json(&[&status_json(
            "1.52.0",
            r#"["tag:server"]"#,
        )]));
        assert!(matches!(tagged.status(), Err(TailscaleError::TaggedDevice)));
    }

    #[test]
    fn apply_refuses_port_collisions_without_mutating_serve() {
        let serve = r#"{"TCP":{"8443":{"HTTPS":true}},"Web":{"hub:8443":{"Handlers":{"/other":{"Proxy":"http://127.0.0.1:9000"}}}}}"#;
        let tailscale = Tailscale::new(FakeRunner::with_json(&[serve, "{}"]));

        assert!(matches!(
            tailscale.apply_audetic_serve(),
            Err(TailscaleError::ServeCollision)
        ));
        assert_eq!(tailscale.runner.calls().len(), 2);
    }

    #[test]
    fn apply_adds_only_the_audetic_mapping_and_never_funnel() {
        let tailscale = Tailscale::new(FakeRunner::with_json(&["{}", "{}", ""]));

        tailscale.apply_audetic_serve().unwrap();

        assert_eq!(
            tailscale.runner.calls(),
            vec![
                vec!["tailscale", "serve", "status", "--json"],
                vec!["tailscale", "funnel", "status", "--json"],
                vec![
                    "tailscale",
                    "serve",
                    "--bg",
                    "--https=8443",
                    "--set-path=/audetic",
                    "http://127.0.0.1:3738"
                ],
            ]
        );
    }

    #[test]
    fn remove_is_idempotent_and_removes_only_the_owned_mapping() {
        let owned = r#"{"TCP":{"8443":{"HTTPS":true}},"Web":{"hub:8443":{"Handlers":{"/audetic":{"Proxy":"http://127.0.0.1:3738"}}}}}"#;
        let tailscale = Tailscale::new(FakeRunner::with_json(&[owned, "{}", ""]));

        assert!(tailscale.remove_audetic_serve().unwrap());
        assert_eq!(
            tailscale.runner.calls()[2],
            vec![
                "tailscale",
                "serve",
                "--https=8443",
                "--set-path=/audetic",
                "off"
            ]
        );
    }

    #[test]
    fn manual_cleanup_command_is_exact_and_never_resets_serve() {
        let command = audetic_serve_cleanup_command();

        assert_eq!(
            command,
            "tailscale serve --https=8443 --set-path=/audetic off"
        );
        assert!(!command.contains("reset"));
    }

    #[test]
    fn funnel_on_the_dedicated_port_fails_closed() {
        let funnel = r#"{"AllowFunnel":{"hub.example.ts.net:8443":true}}"#;
        let tailscale = Tailscale::new(FakeRunner::with_json(&["{}", funnel]));

        assert!(matches!(
            tailscale.apply_audetic_serve(),
            Err(TailscaleError::FunnelEnabled)
        ));
    }

    #[test]
    fn first_use_https_consent_failure_preserves_tailscales_actionable_message() {
        let tailscale = Tailscale::new(FakeRunner::with_outputs(vec![
            CommandOutput {
                status: 0,
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
            },
            CommandOutput {
                status: 0,
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
            },
            CommandOutput {
                status: 1,
                stdout: Vec::new(),
                stderr: b"enable HTTPS at https://login.tailscale.com/admin/dns".to_vec(),
            },
        ]));

        let error = tailscale.apply_audetic_serve().unwrap_err();

        assert!(matches!(
            error,
            TailscaleError::CommandFailed { stderr, .. }
                if stderr == "enable HTTPS at https://login.tailscale.com/admin/dns"
        ));
    }
}
