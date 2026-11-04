//! Harnesses as data (PLAN §5: "a harness is an ACP agent command plus its sandbox profile").
//!
//! A [`DriverConfig`] says how to launch one ACP agent: the command (with placeholders the host fills per
//! session), its environment, how HAN's tools reach it ([`McpMode`]), how it gets model credentials
//! ([`CredentialMode`]), what it needs to reach through the proxy, the adapter version it was tested with
//! (SR-76, SR-77), its gate fidelity (SR-34) and how its configuration is isolated from the user's and the
//! repo's (SR-30). Configs load from TOML or JSON; [`builtin`] ships `pi` and `claude`.
//!
//! [`prepare_session_dir`] fills the placeholders and writes the per-session files the harness reads (pi's
//! `models.json` and `mcp.json` in its own agent directory, never the repo or the user's `~/.pi`).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::acp::{GateFidelity, McpServerSpec};
use crate::sandbox::ShimCommand;

/// How HAN's MCP tools reach the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpMode {
    /// Through ACP `session/new.mcpServers` (claude-agent-acp accepts them).
    AcpMcpServers,
    /// Through pi's own `mcp.json` in the per-session agent directory (pi-acp ignores ACP `mcpServers`).
    PiMcpJson {
        /// pi's tool exposure for the han server: `direct`, so the model sees `mcp__han__propose` (with
        /// pi's default `codemode` the tool is hidden).
        exposure: String,
    },
}

/// Which header carries the injected key on the model route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyHeader {
    /// `x-api-key: <key>` (Anthropic API keys).
    XApiKey,
    /// `authorization: Bearer <token>` (OAuth tokens, OpenAI-style keys).
    Bearer,
}

/// How the harness gets model credentials (PLAN §6.4 Model credentials).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CredentialMode {
    /// The proxy holds the real key and injects it on the model route; the agent holds `dummy_key`.
    ApiKey {
        /// The provider's base URL the proxy forwards to, for example `https://api.anthropic.com`.
        provider_base: String,
        /// The header the key goes in.
        header: KeyHeader,
        /// The dummy key in the provider's format that the agent sees.
        dummy_key: String,
        /// Path prefixes the model route allows (SR-40), for example `/v1/messages`.
        paths: Vec<String>,
        /// The provider id the harness's model config uses (pi's `models.json` key), for example `anthropic`.
        #[serde(default)]
        provider_id: Option<String>,
        /// The pi `api` name (for example `anthropic-messages`), if needed.
        #[serde(default)]
        api: Option<String>,
        /// Models to list in the harness's model config.
        #[serde(default)]
        models: Vec<String>,
        /// A PEM file of the certificate authority the provider's certificate chains to, for a provider with
        /// a private one (a gateway, a test provider). It replaces the system roots for this route.
        #[serde(default)]
        ca_file: Option<std::path::PathBuf>,
    },
    /// The harness keeps its own subscription login; the proxy is CONNECT-only to these hosts and counts
    /// nothing (accounting comes from ACP `_meta.quota` and `usage_update`).
    Subscription {
        /// The hosts the login and the model need.
        connect_hosts: Vec<String>,
    },
}

/// The ACP adapter as installed (SR-76: exact version and integrity).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    /// The npm package.
    pub package: String,
    /// The exact version.
    pub version: String,
    /// The tarball's npm integrity (`sha512-…`); empty until the managed install records it.
    #[serde(default)]
    pub integrity: String,
    /// The script inside the package to run with node (relative to the package directory).
    #[serde(default)]
    pub entry: String,
}

/// How the harness is kept from loading configuration from the repo or the user (SR-30).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Isolation {
    /// An environment variable that points the harness at a per-session config directory.
    #[serde(default)]
    pub config_dir_env: Option<String>,
    /// An environment variable that points the harness at the agent's own login directory (a subscription
    /// login lives in the harness's config directory). The host binds that directory read-write into this
    /// agent's sandbox and nothing else of the user's home (see [`login_binding`]).
    #[serde(default)]
    pub login_dir_env: Option<String>,
    /// Login details supplied by the adapter. Older drivers without this field retain the Claude login
    /// convention; new adapters should always declare it when `login_dir_env` is set.
    #[serde(default)]
    pub login: Option<Login>,
    /// Refuse inherited configuration that this adapter cannot disable itself. Paths are relative to
    /// the workspace or harness login directory. This is an admission check, not a runtime write filter.
    #[serde(default)]
    pub reject_config: ConfigAdmission,
    /// Login-relative directories replaced by empty, writable session-only mounts. Requires a backend
    /// that enforces volatile directories; inherited contents are never loaded or modified.
    #[serde(default)]
    pub volatile_login_dirs: Vec<String>,
    /// Extra arguments for the adapter.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Whether to start with no settings sources (Claude Agent SDK `settingSources: []`).
    #[serde(default)]
    pub settings_sources_empty: bool,
    /// Whether to start with strict MCP configuration (only the servers HAN passes).
    #[serde(default)]
    pub strict_mcp: bool,
    /// What isolation this driver cannot enforce, for the status item.
    #[serde(default)]
    pub gaps: Vec<String>,
}

/// Configuration sources an adapter must not inherit when a session starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigAdmission {
    /// Paths under the workspace, for example an adapter's project configuration directory.
    #[serde(default)]
    pub project: Vec<String>,
    /// Paths under this harness's login directory, never the enclosing agent directory.
    #[serde(default)]
    pub login: Vec<String>,
}

fn portable_relative(p: &str) -> bool {
    !p.contains([':', '\\'])
        && !p.starts_with('/')
        && !p.split('/').any(|c| c == ".." || c == ".")
        && !p.chars().any(char::is_control)
}

/// How to authenticate a harness, independently of its protocol and model provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// A relative directory under the agent's login root. Separates different harnesses' credentials.
    #[serde(default)]
    pub subdir: String,
    /// A nonempty file whose presence indicates a cached login (not proof that the token is still valid).
    pub file: String,
    /// Argument vector run outside the sandbox for interactive login. Supports `{node}`, `{adapter_dir}`
    /// and `{login_dir}`; it is never interpreted by a shell.
    pub command: Vec<String>,
}

impl Default for Login {
    fn default() -> Self {
        // Compatibility with the first shipped drivers, which only declared CLAUDE_CONFIG_DIR.
        Self {
            subdir: String::new(),
            file: CLAUDE_LOGIN_FILE.into(),
            command: vec!["claude".into()],
        }
    }
}

impl Login {
    /// Rejects absolute or traversing credential paths before they are joined to the agent's root.
    pub fn validate(&self) -> Result<(), String> {
        if self.file.is_empty()
            || !portable_relative(&self.file)
            || !portable_relative(&self.subdir)
        {
            return Err(
                "login file and subdir must be portable paths within the agent's login root".into(),
            );
        }
        if self.command.is_empty() || self.command[0].is_empty() {
            return Err("login command must be a nonempty argument vector".into());
        }
        Ok(())
    }

    /// This harness's login directory under the agent's root.
    pub fn directory(&self, root: &Path) -> PathBuf {
        root.join(&self.subdir)
    }

    /// Only checks file metadata; never reads credentials or reports their contents.
    pub fn present(&self, root: &Path) -> bool {
        self.validate().is_ok()
            && std::fs::metadata(self.directory(root).join(&self.file))
                .is_ok_and(|m| m.is_file() && m.len() > 0)
    }
}

/// One harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverConfig {
    /// The driver's name (`pi`, `claude`, or a custom one).
    pub name: String,
    /// The harness label for status (`han.status/1` `harness`).
    pub harness: String,
    /// The command. Placeholders: `{node}`, `{adapter_dir}`, `{agent_dir}`, `{worktree}`,
    /// `{proxy_url}`, `{proxy_port}`, `{proxy_token}`, `{node_flags}` (expands to the flags, one argument
    /// each).
    pub command: Vec<String>,
    /// The environment (same placeholders). The sandbox adds the proxy settings and nothing else.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// How HAN's tools reach the agent.
    pub mcp: McpMode,
    /// How the harness gets model credentials.
    pub credentials: CredentialMode,
    /// Hosts the agent may tunnel to without credentials (registries, docs), on top of the credential mode's.
    #[serde(default)]
    pub connect_hosts: Vec<String>,
    /// The harness version the driver was tested with (SR-77), compared with ACP `agentInfo.version`.
    pub tested_version: String,
    /// The ACP adapter package.
    #[serde(default)]
    pub adapter: Adapter,
    /// Whether the harness's prompts can gate every call.
    pub gate_fidelity: GateFidelity,
    /// Configuration isolation.
    #[serde(default)]
    pub isolation: Isolation,
    /// Flags for node (an AppContainer cannot stat the worktree's ancestors, M4 spike Q2).
    #[serde(default)]
    pub node_flags: Vec<String>,
    /// The ACP session mode for strict turns, when the agent has one.
    #[serde(default)]
    pub strict_mode: Option<String>,
    /// The ACP session mode for auto turns.
    #[serde(default)]
    pub auto_mode: Option<String>,
    /// Where the harness keeps session logs to probe for hidden provider errors (relative to the agent
    /// directory), for example pi's `sessions`.
    #[serde(default)]
    pub session_log_dir: Option<String>,
}

impl DriverConfig {
    /// Validates session-only directory mounts within the selected harness login directory. Only direct
    /// children are accepted; symlinks and non-directories fail closed rather than redirecting a mount.
    pub fn volatile_login_paths(&self, login: Option<&Path>) -> Result<Vec<PathBuf>, String> {
        let mut paths = Vec::new();
        for rel in &self.isolation.volatile_login_dirs {
            if rel.is_empty() || !portable_relative(rel) || rel.contains('/') {
                return Err("volatile login directories must be direct child names".into());
            }
            let root = login.ok_or("volatile login directories need a login directory")?;
            let path = root.join(rel);
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(format!(
                        "volatile login path is not a plain directory: {}",
                        path.display()
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot inspect volatile login directory: {e}")),
            }
            paths.push(path);
        }
        Ok(paths)
    }
    /// Refuses inherited adapter configuration before any harness process starts. Nothing is removed or
    /// rewritten: an operator can inspect the named path and choose a suitable driver.
    pub fn check_configuration(
        &self,
        workspace: &Path,
        login: Option<&Path>,
    ) -> Result<(), String> {
        self.check_configuration_roots(Some(workspace), login)
    }

    /// Checks persistent login state before the host reports admission. Project configuration is checked
    /// separately after the workspace is materialized, before the harness launches.
    pub fn check_login_configuration(&self, login: Option<&Path>) -> Result<(), String> {
        self.volatile_login_paths(login)?;
        self.check_configuration_roots(None, login)
    }

    fn check_configuration_roots(
        &self,
        workspace: Option<&Path>,
        login: Option<&Path>,
    ) -> Result<(), String> {
        let project = workspace.map(|root| (Some(root), &self.isolation.reject_config.project));
        for (root, paths) in project
            .into_iter()
            .chain([(login, &self.isolation.reject_config.login)])
        {
            if paths.is_empty() {
                continue;
            }
            let root = root.ok_or("configuration admission needs a login directory")?;
            for rel in paths {
                if rel.is_empty() || !portable_relative(rel) {
                    return Err(
                        "configuration admission paths must be portable relative paths".into(),
                    );
                }
                let path = root.join(rel);
                match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        return Err(format!(
                            "{} refuses inherited configuration at {}; inspect it or use a driver that isolates this source",
                            self.name,
                            path.display()
                        ));
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(format!(
                            "cannot check configuration at {}: {e}",
                            path.display()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    /// Reads the same TOML/JSON driver used by setup, login, diagnostics and the host.
    pub fn load(path: &Path) -> Result<DriverConfig, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if path.extension().is_some_and(|e| e == "toml") {
            Self::from_toml(&text)
        } else {
            Self::from_json(&text)
        }
    }

    /// The adapter's login contract, including compatibility with existing driver files.
    pub fn login(&self) -> Option<Login> {
        self.isolation
            .login_dir_env
            .as_ref()
            .map(|_| self.isolation.login.clone().unwrap_or_default())
    }
    /// Parses a config from TOML.
    pub fn from_toml(text: &str) -> Result<DriverConfig, String> {
        let cfg: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(login) = cfg.login() {
            login.validate()?;
        }
        Ok(cfg)
    }

    /// Parses a config from JSON.
    pub fn from_json(text: &str) -> Result<DriverConfig, String> {
        let cfg: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if let Some(login) = cfg.login() {
            login.validate()?;
        }
        Ok(cfg)
    }

    /// Every host the proxy must allow for this harness: the credential mode's plus `connect_hosts`.
    pub fn allow_hosts(&self) -> Vec<String> {
        let mut hosts = match &self.credentials {
            CredentialMode::Subscription { connect_hosts } => connect_hosts.clone(),
            CredentialMode::ApiKey { .. } => Vec::new(),
        };
        hosts.extend(self.connect_hosts.iter().cloned());
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// The `_meta` for ACP `session/new` that applies the isolation (claude-agent-acp reads
    /// `claudeCode.options`, which override its defaults).
    pub fn session_meta(&self) -> Option<Value> {
        let mut options = serde_json::Map::new();
        if self.isolation.settings_sources_empty {
            options.insert("settingSources".into(), json!([]));
        }
        if self.isolation.strict_mcp {
            options.insert("strictMcpConfig".into(), json!(true));
        }
        if !options.is_empty() {
            // Never let the session drop permission prompts: strict turns need them.
            options.insert("allowDangerouslySkipPermissions".into(), json!(false));
        }
        (!options.is_empty()).then(|| json!({"claudeCode": {"options": options}}))
    }
}

/// The shipped drivers: `pi`, `claude` and `codex`.
pub fn builtin(name: &str) -> Option<DriverConfig> {
    match name {
        "pi" => Some(pi()),
        "claude" | "claude-code" => Some(claude()),
        "codex" => Some(
            DriverConfig::from_json(include_str!("../drivers/codex.json"))
                .expect("the shipped Codex driver is valid"),
        ),
        _ => None,
    }
}

/// The registries and docs every harness may reach without credentials (SR-38 default allow-list). The crates.io
/// hosts are not here: they come with the Rust toolchain ([`crate::toolchain`]), only when it is enabled.
pub fn default_connect_hosts() -> Vec<String> {
    ["registry.npmjs.org", "pypi.org", "files.pythonhosted.org"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn pi() -> DriverConfig {
    DriverConfig {
        name: "pi".into(),
        harness: "pi".into(),
        // Launched through node, never a .cmd shim (cmd.exe breaks inside an AppContainer, spike Q2).
        command: vec![
            "{node}".into(),
            "{node_flags}".into(),
            "{adapter_dir}/dist/index.js".into(),
        ],
        env: vec![
            ("PI_CODING_AGENT_DIR".into(), "{agent_dir}".into()),
            ("PI_ACP_PI_COMMAND".into(), "{pi_command}".into()),
        ],
        mcp: McpMode::PiMcpJson {
            exposure: "direct".into(),
        },
        credentials: CredentialMode::ApiKey {
            provider_base: "https://api.anthropic.com".into(),
            header: KeyHeader::XApiKey,
            dummy_key: "han-dummy-key-not-a-secret".into(),
            paths: vec!["/v1/messages".into()],
            provider_id: Some("anthropic".into()),
            api: Some("anthropic-messages".into()),
            models: vec!["claude-sonnet-5-5".into()],
            ca_file: None,
        },
        connect_hosts: default_connect_hosts(),
        tested_version: "0.0.34".into(),
        adapter: Adapter {
            package: "pi-acp".into(),
            version: "0.0.34".into(),
            integrity: "sha512-MrCp37UbpUzkTkzTIMJQf1RYrT+kr4fgGDGFoLmBN26UupuCZp0XxVIY+7XYsMo3yDnzDeZUJ0jPyM3Si6Bkkg==".into(),
            entry: "dist/index.js".into(),
        },
        gate_fidelity: GateFidelity::PromptOnly,
        isolation: Isolation {
            config_dir_env: Some("PI_CODING_AGENT_DIR".into()),
            login_dir_env: None,
            login: None,
            reject_config: ConfigAdmission::default(),
            volatile_login_dirs: Vec::new(),
            extra_args: vec![],
            settings_sources_empty: false,
            strict_mcp: false,
            gaps: vec![
                "pi has no permission prompts: the sandbox is the only gate (prompt_only)".into(),
            ],
        },
        node_flags: vec![
            "--preserve-symlinks".into(),
            "--preserve-symlinks-main".into(),
        ],
        strict_mode: None,
        auto_mode: None,
        session_log_dir: Some("sessions".into()),
    }
}

fn claude() -> DriverConfig {
    DriverConfig {
        name: "claude".into(),
        harness: "claude-code".into(),
        command: vec![
            "{node}".into(),
            "{node_flags}".into(),
            "{adapter_dir}/dist/index.js".into(),
        ],
        env: vec![],
        mcp: McpMode::AcpMcpServers,
        credentials: CredentialMode::Subscription {
            connect_hosts: vec![
                "api.anthropic.com".into(),
                "platform.claude.com".into(),
                "claude.ai".into(),
            ],
        },
        connect_hosts: default_connect_hosts(),
        tested_version: "0.89.1".into(),
        adapter: Adapter {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.89.1".into(),
            integrity: "sha512-t1k8YPDvo/zUghL3x2pFs40tWfYTLhYmK8jt37CVBpY7eq1Ztj2hLZBAmy9ZX++tUyJaGfuuMrzvJGvU9tjBsg==".into(),
            entry: "dist/index.js".into(),
        },
        gate_fidelity: GateFidelity::EveryCall,
        isolation: Isolation {
            // Claude Code's login lives in its config directory, so the subscription mode cannot point it at
            // a fresh one per session; it gets one per agent instead (`login_dir_env`), and settingSources: []
            // and strictMcpConfig keep user and project settings and MCP servers out.
            config_dir_env: None,
            login_dir_env: Some("CLAUDE_CONFIG_DIR".into()),
            login: Some(Login::default()),
            reject_config: ConfigAdmission::default(),
            volatile_login_dirs: Vec::new(),
            extra_args: vec![],
            settings_sources_empty: true,
            strict_mcp: true,
            gaps: vec![
                "per-agent login dir inside the agent's trust boundary: the agent can read and use its own Claude login (CLAUDE_CONFIG_DIR is bound read-write into this sandbox only; nothing else of the user's home is)"
                    .into(),
                "a skill or setting placed in the agent's login dir may still load (spike Q4)".into(),
            ],
        },
        node_flags: vec![
            "--preserve-symlinks".into(),
            "--preserve-symlinks-main".into(),
        ],
        strict_mode: Some("default".into()),
        auto_mode: Some("acceptEdits".into()),
        session_log_dir: None,
    }
}

/// The file Claude Code writes its subscription login to, inside its config directory.
pub const CLAUDE_LOGIN_FILE: &str = ".credentials.json";

/// Whether `dir` holds a login (never reads or returns its contents).
pub fn login_present(dir: &Path) -> bool {
    std::fs::metadata(dir.join(CLAUDE_LOGIN_FILE)).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// The per-agent login directory a driver needs bound into its sandbox: the environment variable that names
/// it, and the absolute directory (created if missing). `None` for a driver with no subscription login. It is
/// an error when the driver needs one and the agent has none (`han agent login <agent>` makes it).
pub fn login_binding(
    cfg: &DriverConfig,
    dir: Option<&Path>,
) -> Result<Option<(String, PathBuf)>, String> {
    let Some(var) = &cfg.isolation.login_dir_env else {
        return Ok(None);
    };
    let login = cfg
        .login()
        .expect("a login environment has a login contract");
    login.validate()?;
    let dir = dir.ok_or_else(|| {
        format!(
            "{} needs a login directory for the agent (run `han agent login <agent>`)",
            cfg.name
        )
    })?;
    let root = dir;
    let dir = login.directory(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !login.present(root) {
        return Err(format!(
            "no {} login in {} (run `han agent login <agent>`)",
            cfg.name,
            dir.display()
        ));
    }
    Ok(Some((var.clone(), dir)))
}

/// Adds a login binding to a launch: the variable to the environment and the directory to the read-write
/// paths. This is the only thing of the user's home the sandbox gets.
pub fn bind_login(
    binding: Option<(String, PathBuf)>,
    env: &mut Vec<(String, String)>,
    rw: &mut Vec<PathBuf>,
) {
    if let Some((var, dir)) = binding {
        env.push((var, dir.to_string_lossy().into_owned()));
        rw.push(dir);
    }
}

/// What the host knows when it prepares a session's launch.
#[derive(Clone, Debug)]
pub struct PrepareCtx {
    /// The node executable.
    pub node: PathBuf,
    /// The installed adapter package's directory.
    pub adapter_dir: PathBuf,
    /// The per-session agent directory (inside the sandbox's writable area, not the worktree).
    pub agent_dir: PathBuf,
    /// The session's worktree.
    pub worktree: PathBuf,
    /// The in-sandbox shim (runs `mcp-stub`).
    pub shim: ShimCommand,
    /// The proxy port the agent reaches (inside the sandbox: the shim's loopback port).
    pub proxy_port: u16,
    /// The per-session proxy token (SR-28).
    pub proxy_token: String,
    /// A pi executable (`PI_ACP_PI_COMMAND`), when not the default.
    pub pi_command: Option<PathBuf>,
    /// The name of HAN's MCP server (tools appear as `mcp__<name>__…`), default `han`.
    pub mcp_name: Option<String>,
}

impl PrepareCtx {
    /// The proxy URL inside the sandbox.
    pub fn proxy_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.proxy_port)
    }

    fn vars(&self, cfg: &DriverConfig) -> BTreeMap<&'static str, String> {
        let p = |x: &Path| x.to_string_lossy().into_owned();
        let mut m = BTreeMap::new();
        m.insert("node", p(&self.node));
        m.insert("adapter_dir", p(&self.adapter_dir));
        m.insert("agent_dir", p(&self.agent_dir));
        m.insert("worktree", p(&self.worktree));
        m.insert("proxy_url", self.proxy_url());
        m.insert("proxy_port", self.proxy_port.to_string());
        m.insert("proxy_token", self.proxy_token.clone());
        m.insert(
            "pi_command",
            self.pi_command
                .as_deref()
                .map(p)
                .unwrap_or_else(|| "pi".into()),
        );
        m.insert("node_flags", cfg.node_flags.join(" "));
        m
    }
}

/// Replaces `{name}` placeholders.
pub fn fill(template: &str, vars: &BTreeMap<&'static str, String>) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// What [`prepare_session_dir`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLaunch {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// The driver's environment (the sandbox adds proxy settings).
    pub env: Vec<(String, String)>,
    /// Files written into the agent directory.
    pub files_written: Vec<PathBuf>,
    /// MCP servers to pass through ACP (for [`McpMode::AcpMcpServers`]).
    pub mcp_servers: Vec<McpServerSpec>,
    /// `_meta` for `session/new`.
    pub session_meta: Option<Value>,
}

/// The han MCP server entry: the in-sandbox shim's `mcp-stub`, tunnelling stdio over the session proxy to
/// han-mcp outside the sandbox (SR-29).
pub fn han_mcp_server(ctx: &PrepareCtx) -> McpServerSpec {
    McpServerSpec::Stdio {
        name: ctx.mcp_name.clone().unwrap_or_else(|| "han".into()),
        command: ctx.shim.program.clone(),
        args: ctx.shim.args([
            "mcp-stub".into(),
            "--proxy".into(),
            format!("127.0.0.1:{}", ctx.proxy_port),
            "--token".into(),
            ctx.proxy_token.clone(),
        ]),
        env: vec![],
    }
}

/// Fills a driver's placeholders and writes the per-session files it reads.
pub fn prepare_session_dir(cfg: &DriverConfig, ctx: &PrepareCtx) -> io::Result<PreparedLaunch> {
    let vars = ctx.vars(cfg);
    let mut argv: Vec<String> = Vec::new();
    for part in &cfg.command {
        if part == "{node_flags}" {
            argv.extend(cfg.node_flags.iter().cloned());
        } else {
            argv.push(fill(part, &vars));
        }
    }
    argv.extend(cfg.isolation.extra_args.iter().map(|a| fill(a, &vars)));
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }
    let program = PathBuf::from(argv.remove(0));
    let mut env: Vec<(String, String)> = cfg
        .env
        .iter()
        .map(|(k, v)| (k.clone(), fill(v, &vars)))
        .collect();
    if let Some(var) = &cfg.isolation.config_dir_env
        && !env.iter().any(|(k, _)| k == var)
    {
        env.push((var.clone(), ctx.agent_dir.to_string_lossy().into_owned()));
    }
    std::fs::create_dir_all(&ctx.agent_dir)?;
    let mut files_written = Vec::new();
    let mut mcp_servers = Vec::new();
    match &cfg.mcp {
        McpMode::AcpMcpServers => mcp_servers.push(han_mcp_server(ctx)),
        McpMode::PiMcpJson { exposure } => {
            let McpServerSpec::Stdio {
                name,
                command,
                args,
                ..
            } = han_mcp_server(ctx)
            else {
                unreachable!("the han server is a stdio entry")
            };
            let doc = json!({"mcpServers": {name: {"command": command, "args": args, "exposure": exposure}}});
            let path = ctx.agent_dir.join("mcp.json");
            write_json(&path, &doc)?;
            files_written.push(path);
        }
    }
    if let CredentialMode::ApiKey {
        dummy_key,
        provider_id,
        api,
        models,
        ..
    } = &cfg.credentials
        && cfg.name == "pi"
    {
        // pi's per-session models.json: the base URL is the proxy, the key a dummy, and the session token
        // a header the proxy checks and strips (M4 spike Q3).
        let provider = provider_id.clone().unwrap_or_else(|| "anthropic".into());
        let mut p = json!({
            "baseUrl": ctx.proxy_url(),
            "apiKey": dummy_key,
            "headers": {"x-han-session": ctx.proxy_token},
        });
        if let Some(api) = api {
            p["api"] = json!(api);
        }
        if !models.is_empty() {
            p["models"] = json!(
                models
                    .iter()
                    .map(|m| json!({"id": m, "name": m}))
                    .collect::<Vec<_>>()
            );
        }
        let doc = json!({"providers": {provider: p}});
        let path = ctx.agent_dir.join("models.json");
        write_json(&path, &doc)?;
        files_written.push(path);
    }
    Ok(PreparedLaunch {
        program,
        args: argv,
        env,
        files_written,
        mcp_servers,
        session_meta: cfg.session_meta(),
    })
}

fn write_json(path: &Path, doc: &Value) -> io::Result<()> {
    let text = serde_json::to_string_pretty(doc).map_err(io::Error::other)?;
    std::fs::write(path, text)
}

/// The outcome of the version check (SR-77).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum VersionCheck {
    /// The agent reports the tested version.
    Tested,
    /// It reports another (or none); the host warns, and a strict space SHOULD refuse.
    Untested {
        /// What to show.
        warning: String,
    },
}

/// Compares ACP `agentInfo.version` with the driver's tested version (SR-77).
pub fn check_version(cfg: &DriverConfig, reported: Option<&str>) -> VersionCheck {
    match reported {
        Some(v) if v == cfg.tested_version => VersionCheck::Tested,
        Some(v) => VersionCheck::Untested {
            warning: format!(
                "{} reports version {v}; the driver was tested with {}",
                cfg.name, cfg.tested_version
            ),
        },
        None => VersionCheck::Untested {
            warning: format!(
                "{} reports no version; the driver was tested with {}",
                cfg.name, cfg.tested_version
            ),
        },
    }
}

/// pi-acp hides provider errors (spike Q1): the newest session JSONL under `dir` (searched two levels deep)
/// says `stopReason: "error"` with an `errorMessage`. Returns that message when the last such record is an
/// error.
pub fn pi_session_error(dir: &Path) -> Option<String> {
    fn newest(dir: &Path, depth: u32, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() && depth > 0 {
                newest(&path, depth - 1, best);
            } else if path.extension().is_some_and(|x| x == "jsonl")
                && let Ok(m) = e.metadata().and_then(|m| m.modified())
                && best.as_ref().is_none_or(|(t, _)| m > *t)
            {
                *best = Some((m, path));
            }
        }
    }
    let mut best = None;
    newest(dir, 2, &mut best);
    let (_, path) = best?;
    let text = std::fs::read_to_string(path).ok()?;
    let last = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            let msg = if v.get("message").is_some() {
                &v["message"]
            } else {
                &v
            };
            msg.get("stopReason").and_then(Value::as_str).map(|s| {
                (
                    s.to_owned(),
                    msg.get("errorMessage")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })
        })?;
    (last.0 == "error").then(|| last.1.unwrap_or_else(|| "provider error".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_codex_is_the_shipped_data_descriptor() {
        let named = builtin("codex").expect("guided setup can resolve its default harness");
        let file = DriverConfig::from_json(include_str!("../drivers/codex.json")).unwrap();
        assert_eq!(
            serde_json::to_value(named).unwrap(),
            serde_json::to_value(file).unwrap()
        );
    }

    #[test]
    fn no_shipped_driver_allows_a_registry_a_toolchain_owns() {
        // crates.io comes only with the Rust toolchain (SR-105).
        for name in ["pi", "claude"] {
            let hosts = builtin(name).unwrap().allow_hosts();
            assert!(
                !hosts.iter().any(|h| h.contains("crates.io")),
                "{name}: {hosts:?}"
            );
        }
        assert!(
            !default_connect_hosts()
                .iter()
                .any(|h| h.contains("crates.io"))
        );
    }

    fn ctx(dir: &Path) -> PrepareCtx {
        PrepareCtx {
            node: PathBuf::from("/rt/node"),
            adapter_dir: PathBuf::from("/rt/pi-acp"),
            agent_dir: dir.join("agent"),
            worktree: dir.join("wt"),
            shim: ShimCommand::new("/rt/han", ["shim"]),
            proxy_port: 18090,
            proxy_token: "tok123".into(),
            pi_command: None,
            mcp_name: None,
        }
    }

    #[test]
    fn builtins_roundtrip_through_toml_and_json() {
        for name in ["pi", "claude"] {
            let cfg = builtin(name).unwrap();
            let t = toml::to_string(&cfg).unwrap();
            assert_eq!(DriverConfig::from_toml(&t).unwrap(), cfg);
            let j = serde_json::to_string(&cfg).unwrap();
            assert_eq!(DriverConfig::from_json(&j).unwrap(), cfg);
        }
    }

    #[test]
    fn pi_session_dir_holds_a_dummy_key_and_the_direct_han_server() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = builtin("pi").unwrap();
        let p = prepare_session_dir(&cfg, &ctx(dir.path())).unwrap();
        assert_eq!(p.program, PathBuf::from("/rt/node"));
        assert_eq!(
            p.args,
            vec![
                "--preserve-symlinks",
                "--preserve-symlinks-main",
                "/rt/pi-acp/dist/index.js"
            ]
        );
        assert!(p.mcp_servers.is_empty());
        let mcp: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("agent/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mcp["mcpServers"]["han"]["exposure"], "direct");
        // the shim's own prefix first (`han shim`), then its verb
        assert_eq!(mcp["mcpServers"]["han"]["args"][0], "shim");
        assert_eq!(mcp["mcpServers"]["han"]["args"][1], "mcp-stub");
        let models: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("agent/models.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            models["providers"]["anthropic"]["baseUrl"],
            "http://127.0.0.1:18090"
        );
        assert_eq!(
            models["providers"]["anthropic"]["headers"]["x-han-session"],
            "tok123"
        );
        assert!(
            models["providers"]["anthropic"]["apiKey"]
                .as_str()
                .unwrap()
                .contains("dummy")
        );
        assert!(
            p.env
                .iter()
                .any(|(k, v)| k == "PI_CODING_AGENT_DIR" && v.ends_with("agent"))
        );
    }

    #[test]
    fn claude_gets_its_own_login_dir_and_pi_none() {
        let dir = tempfile::tempdir().unwrap();
        let login = dir.path().join("claude-config");
        let cfg = builtin("claude").unwrap();
        // no login dir given, or none logged in yet: the session does not start
        assert!(login_binding(&cfg, None).unwrap_err().contains("login"));
        assert!(!login_present(&login));
        assert!(
            login_binding(&cfg, Some(&login))
                .unwrap_err()
                .contains("han agent login")
        );
        std::fs::write(login.join(CLAUDE_LOGIN_FILE), "{}").unwrap();
        let (var, bound) = login_binding(&cfg, Some(&login)).unwrap().unwrap();
        assert_eq!(var, "CLAUDE_CONFIG_DIR");
        assert!(bound.is_absolute() && bound.ends_with("claude-config"));
        assert!(
            cfg.isolation
                .gaps
                .iter()
                .any(|g| g.contains("per-agent login dir inside the agent's trust boundary"))
        );
        assert_eq!(login_binding(&builtin("pi").unwrap(), None).unwrap(), None);
    }

    #[test]
    fn a_file_driver_selects_its_own_credentials_without_exposing_another_harness() {
        let root = tempfile::tempdir().unwrap();
        let cfg = DriverConfig::from_json(include_str!("../drivers/codex.json")).unwrap();
        // A valid Claude credential file must not satisfy a different adapter's login check.
        std::fs::write(root.path().join(CLAUDE_LOGIN_FILE), "{}").unwrap();
        assert!(login_binding(&cfg, Some(root.path())).is_err());
        let login = cfg.login().unwrap();
        let codex = login.directory(root.path());
        std::fs::write(codex.join("auth.json"), "").unwrap();
        assert!(!login.present(root.path()));
        std::fs::write(codex.join("auth.json"), "{}").unwrap();
        let (env, bound) = login_binding(&cfg, Some(root.path())).unwrap().unwrap();
        assert_eq!(env, "CODEX_HOME");
        assert_eq!(bound, std::path::absolute(&codex).unwrap());
        let mut env = Vec::new();
        let mut rw = Vec::new();
        bind_login(
            Some(("CODEX_HOME".into(), bound.clone())),
            &mut env,
            &mut rw,
        );
        assert_eq!(rw, [bound]);
        assert!(!rw.contains(&root.path().to_path_buf()));
        let prepared = prepare_session_dir(&cfg, &ctx(root.path())).unwrap();
        assert_eq!(prepared.mcp_servers.len(), 1);
        assert_eq!(prepared.session_meta, None);
        assert_eq!(cfg.gate_fidelity, GateFidelity::PromptOnly);
    }

    #[test]
    fn login_paths_cannot_escape_the_agents_root_on_either_os() {
        for path in [
            "../token",
            "/token",
            "C:/token",
            "C:token",
            "a/../token",
            "a\\token",
            "a\ntoken",
        ] {
            let mut cfg = builtin("claude").unwrap();
            cfg.isolation.login.as_mut().unwrap().file = path.into();
            assert!(
                DriverConfig::from_json(&serde_json::to_string(&cfg).unwrap()).is_err(),
                "{path}"
            );
            cfg.isolation.login.as_mut().unwrap().file = "auth.json".into();
            cfg.isolation.login.as_mut().unwrap().subdir = path.into();
            assert!(
                DriverConfig::from_json(&serde_json::to_string(&cfg).unwrap()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn old_driver_files_keep_their_login_contract() {
        let mut old = serde_json::to_value(builtin("claude").unwrap()).unwrap();
        old["isolation"].as_object_mut().unwrap().remove("login");
        let cfg = DriverConfig::from_json(&old.to_string()).unwrap();
        assert_eq!(cfg.login(), Some(Login::default()));
    }

    #[test]
    fn volatile_login_state_survives_admission_without_being_trusted() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = DriverConfig::from_json(include_str!("../drivers/codex.json")).unwrap();
        std::fs::create_dir_all(root.path().join("skills/.system")).unwrap();
        std::fs::write(
            root.path().join("skills/.system/SKILL.md"),
            "untrusted inherited text",
        )
        .unwrap();
        cfg.check_login_configuration(Some(root.path())).unwrap();
        assert_eq!(
            cfg.volatile_login_paths(Some(root.path())).unwrap(),
            vec![root.path().join("skills"), root.path().join("plugins")]
        );
        assert!(root.path().join("skills/.system/SKILL.md").exists());
        for name in ["", "..", "nested/path", "C:escape", "../escape"] {
            cfg.isolation.volatile_login_dirs = vec![name.into()];
            assert!(
                cfg.volatile_login_paths(Some(root.path())).is_err(),
                "{name}"
            );
        }
        cfg.isolation.volatile_login_dirs = vec!["file".into()];
        std::fs::write(root.path().join("file"), "x").unwrap();
        assert!(cfg.volatile_login_paths(Some(root.path())).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn volatile_login_mounts_refuse_symlinks() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("skills")).unwrap();
        let cfg = DriverConfig::from_json(include_str!("../drivers/codex.json")).unwrap();
        assert!(cfg.check_login_configuration(Some(root.path())).is_err());
    }

    #[test]
    fn adapter_admission_refuses_inherited_config_without_removing_it() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("work");
        let login = root.path().join("login");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&login).unwrap();
        let cfg = DriverConfig::from_json(include_str!("../drivers/codex.json")).unwrap();
        cfg.check_configuration(&workspace, Some(&login)).unwrap();
        std::fs::create_dir(workspace.join(".codex")).unwrap();
        let err = cfg
            .check_configuration(&workspace, Some(&login))
            .unwrap_err();
        assert!(err.contains(".codex"), "{err}");
        assert!(workspace.join(".codex").exists());
        std::fs::remove_dir(workspace.join(".codex")).unwrap();
        std::fs::write(login.join("config.toml"), "[mcp_servers.foreign]").unwrap();
        assert!(
            cfg.check_configuration(&workspace, Some(&login))
                .unwrap_err()
                .contains("config.toml")
        );
        // A local login file alone is expected, not treated as configuration.
        std::fs::remove_file(login.join("config.toml")).unwrap();
        std::fs::write(login.join("auth.json"), "{}").unwrap();
        cfg.check_configuration(&workspace, Some(&login)).unwrap();
    }

    #[test]
    fn claude_takes_han_over_acp_with_isolated_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = builtin("claude").unwrap();
        let p = prepare_session_dir(&cfg, &ctx(dir.path())).unwrap();
        assert_eq!(p.mcp_servers.len(), 1);
        let meta = p.session_meta.unwrap();
        assert_eq!(meta["claudeCode"]["options"]["settingSources"], json!([]));
        assert_eq!(meta["claudeCode"]["options"]["strictMcpConfig"], true);
        assert!(!dir.path().join("agent/models.json").exists());
        assert_eq!(
            cfg.allow_hosts()
                .iter()
                .filter(|h| h.contains("anthropic") || h.contains("claude"))
                .count(),
            3
        );
    }

    #[test]
    fn version_check() {
        let cfg = builtin("pi").unwrap();
        assert_eq!(check_version(&cfg, Some("0.0.34")), VersionCheck::Tested);
        assert!(matches!(
            check_version(&cfg, Some("0.0.35")),
            VersionCheck::Untested { .. }
        ));
    }

    #[test]
    fn pi_error_in_session_log() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions/x")).unwrap();
        std::fs::write(
            dir.path().join("sessions/x/s.jsonl"),
            "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"400 out of extra usage\"}}\n",
        )
        .unwrap();
        assert_eq!(
            pi_session_error(&dir.path().join("sessions")).as_deref(),
            Some("400 out of extra usage")
        );
    }
}
