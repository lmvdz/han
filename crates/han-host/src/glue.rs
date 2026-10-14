//! Wiring of one session's egress, sandbox and mirror: the proxy with the driver's allow-list and model
//! route, han-mcp served on the proxy's tunnel, the driver's session directory, and the confined launch.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use han_mcp::HostBridge;
use han_node::api::{Fidelity, Sandbox, SandboxBackend};
use han_vcs::Workspace;

use crate::acp::McpServerSpec;
use crate::driver::{
    CredentialMode, KeyHeader, PrepareCtx, bind_login, login_binding, prepare_session_dir,
};
use crate::host::HostConfig;
use crate::mask::Masker;
use crate::proxy::{
    AllowEntry, KeySource, KeyStyle, ModelRoute, Proxy, ProxyConfig, ProxyHandle, Roots,
};
use crate::sandbox::{self, Limits, Preference, ProxyTarget, SandboxSpec, Sandboxed};
use crate::stderr_log::{self, StderrLog};

/// The session's egress: its proxy and the token the agent presents.
pub struct Egress {
    proxy: ProxyHandle,
    token: String,
    session: String,
}

/// A launched agent.
pub struct Launched {
    /// Its stdin.
    pub stdin: sandbox::ChildIn,
    /// Its stdout.
    pub stdout: sandbox::ChildOut,
    /// The sandbox, for status.
    pub sandbox: Sandbox,
    /// What the sandbox does not enforce.
    pub gaps: Vec<String>,
    /// HAN's MCP servers to pass over ACP.
    pub mcp_servers: Vec<McpServerSpec>,
    /// `_meta` for `session/new`.
    pub session_meta: Option<serde_json::Value>,
    /// The confined tree.
    pub child: Sandboxed,
    /// The agent's stderr, masked, in the session directory (never published).
    pub stderr: Option<StderrLog>,
}

impl Launched {
    /// Kills the agent's tree.
    pub fn kill(&self) {
        self.child.kill();
    }
}

fn preference(name: &str) -> Result<Preference, String> {
    Ok(match name {
        "auto" | "" => Preference::Auto,
        "mxc" => Preference::Mxc,
        "bwrap" => Preference::Bwrap,
        "none" => Preference::None,
        other => return Err(format!("unknown sandbox {other}: auto, mxc, bwrap or none")),
    })
}

/// What the sandbox must let an installed adapter read: the outermost `node_modules` above its package
/// directory, where npm put its dependencies (and, for Claude Code, the `claude` executable the Agent SDK
/// starts). Node resolves a package's imports through every `node_modules` above it, so the package directory
/// alone is not enough. An adapter outside any `node_modules` gets its own directory.
pub fn adapter_read_root(adapter_dir: &Path) -> PathBuf {
    adapter_dir
        .ancestors()
        .filter(|a| a.file_name().is_some_and(|n| n == "node_modules"))
        .last()
        .unwrap_or(adapter_dir)
        .to_path_buf()
}

/// The backend `Auto` resolves to on this machine.
fn resolve(p: Preference) -> Preference {
    if p != Preference::Auto {
        return p;
    }
    let available = sandbox::probe();
    let has = |b: SandboxBackend| available.iter().any(|a| a.backend == b && a.available);
    if cfg!(windows) && has(SandboxBackend::Mxc) {
        Preference::Mxc
    } else if cfg!(target_os = "linux") && has(SandboxBackend::Bwrap) {
        Preference::Bwrap
    } else {
        Preference::None
    }
}

impl Egress {
    /// Starts the session's proxy: the driver's CONNECT allow-list, the model route in API-key mode, and
    /// han-mcp on the tunnel.
    pub async fn start(
        cfg: &HostConfig,
        bridge: &Arc<dyn HostBridge>,
        token: &str,
        _workspace: &Workspace,
        _agent_dir: &Path,
        _temp: &Path,
    ) -> Result<Egress, String> {
        let session = bridge.session();
        let mut pc = ProxyConfig::new(session.clone(), token.to_owned());
        let mut hosts = cfg.driver.allow_hosts();
        hosts.extend(cfg.sandbox.allow_hosts.iter().cloned());
        pc.allow = hosts.into_iter().map(AllowEntry::https).collect();
        if let CredentialMode::ApiKey {
            provider_base,
            header,
            paths,
            ..
        } = &cfg.driver.credentials
        {
            let key = cfg
                .model_key
                .clone()
                .ok_or("API-key mode needs the model key (it stays in the proxy)")?;
            pc.model = Some(ModelRoute {
                name: "model".into(),
                base: provider_base.clone(),
                paths: paths.clone(),
                style: match header {
                    KeyHeader::XApiKey => KeyStyle::XApiKey,
                    KeyHeader::Bearer => KeyStyle::Bearer,
                },
                key: KeySource::new(key),
                roots: if cfg.sandbox.test_roots.is_empty() {
                    Roots::System
                } else {
                    Roots::Only(cfg.sandbox.test_roots.clone())
                },
            });
        }
        pc.allow_private_upstreams = cfg.sandbox.test_loopback_upstream;
        let b = Arc::clone(bridge);
        pc.mcp = Some(Arc::new(move |stream| {
            let b = Arc::clone(&b);
            tokio::spawn(async move {
                if let Err(e) = han_mcp::serve(stream, b).await {
                    tracing::debug!("han-mcp session ended: {e}");
                }
            });
        }));
        let proxy = Proxy::start(pc).await.map_err(|e| e.to_string())?;
        Ok(Egress {
            proxy,
            token: token.to_owned(),
            session,
        })
    }

    /// The proxy, for usage and decisions.
    pub fn proxy(&self) -> &ProxyHandle {
        &self.proxy
    }

    /// Prepares the driver's session directory and launches the agent confined (SR-23: an unavailable
    /// sandbox fails the session unless the space allows advisory mode). The agent's stderr goes, masked, to
    /// [`stderr_log::LOG_NAME`] in `cfg.session_dir`, which the sandbox cannot write.
    pub async fn launch(
        &self,
        cfg: &HostConfig,
        agent_dir: &Path,
        workspace: &Workspace,
        masker: Arc<Masker>,
    ) -> Result<Launched, String> {
        let backend = resolve(preference(&cfg.sandbox.backend)?);
        // Inside a sandbox the agent reaches the proxy through the shim's loopback port; without one, the
        // proxy's own port.
        let inside_port = match backend {
            Preference::None => None,
            _ => Some(sandbox::pick_inside_port().map_err(|e| e.to_string())?),
        };
        let port = inside_port.unwrap_or_else(|| self.proxy.addr().port());
        let login = login_binding(&cfg.driver, cfg.sandbox.login_dir.as_deref())?;
        let prepared = prepare_session_dir(
            &cfg.driver,
            &PrepareCtx {
                node: cfg.node.clone(),
                adapter_dir: cfg.adapter_dir.clone(),
                agent_dir: agent_dir.to_path_buf(),
                worktree: workspace.dir.clone(),
                shim: cfg.shim.clone(),
                proxy_port: port,
                proxy_token: self.token.clone(),
                pi_command: None,
                mcp_name: None,
            },
        )
        .map_err(|e| format!("driver: {e}"))?;
        let temp = agent_dir.parent().unwrap_or(agent_dir).join("tmp");
        let mut ro: Vec<PathBuf> = vec![prepared.program.clone(), cfg.shim.clone()];
        if cfg.node.is_absolute() {
            ro.push(cfg.node.clone());
        }
        if !cfg.adapter_dir.as_os_str().is_empty() {
            ro.push(adapter_read_root(&cfg.adapter_dir));
        }
        if let Some(store) = &workspace.shared_store {
            ro.push(store.clone());
        }
        ro.extend(cfg.sandbox.ro.iter().cloned());
        let mut env = prepared.env.clone();
        let mut rw = vec![workspace.dir.clone(), agent_dir.to_path_buf(), temp.clone()];
        bind_login(login, &mut env, &mut rw);
        let spec = SandboxSpec {
            session: self.session.clone(),
            backend,
            program: prepared.program.clone(),
            args: prepared.args.clone(),
            cwd: workspace.dir.clone(),
            env,
            rw,
            ro,
            temp,
            proxy: Some(ProxyTarget {
                addr: self.proxy.addr(),
                token: self.token.clone(),
            }),
            allow_advisory: cfg.sandbox.allow_advisory,
            limits: Limits {
                memory_bytes: None,
                processes: None,
                wall_time: None,
            },
            shim: Some(cfg.shim.clone()),
            inside_port,
        };
        let mut child = sandbox::launch(spec).await.map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("the agent has no stdin")?;
        let stdout = child.stdout.take().ok_or("the agent has no stdout")?;
        // Drained so it never blocks; kept for diagnosis, never published (it can hold anything).
        let stderr = child.stderr.take().map(|err| {
            stderr_log::capture(err, cfg.session_dir.join(stderr_log::LOG_NAME), masker)
        });
        let mut gaps = child.info.gaps.clone();
        gaps.extend(cfg.driver.isolation.gaps.iter().cloned());
        if !child.info.dns_blocked && child.info.fidelity == Fidelity::Enforced {
            gaps.push("DNS lookups reach the host resolver (SR-27)".into());
        }
        gaps.sort();
        gaps.dedup();
        Ok(Launched {
            stdin,
            stdout,
            sandbox: Sandbox {
                backend: child.info.backend,
                fidelity: child.info.fidelity,
            },
            gaps,
            mcp_servers: prepared.mcp_servers.clone(),
            session_meta: prepared.session_meta.clone(),
            child,
            stderr,
        })
    }
}

/// The handle the runner holds on the worktree mirror.
pub struct MirrorHandle {
    mirror: crate::mirror::Mirror,
}

impl MirrorHandle {
    /// Sets the tool call the next changes are attributed to.
    pub fn set_step(&self, step: Option<String>) {
        self.mirror.set_step(step);
    }

    /// Records everything that changed (turn end, before proposing).
    pub fn rescan(&self) {
        if let Err(e) = self.mirror.rescan() {
            tracing::warn!("mirror rescan: {e}");
        }
    }

    /// The mirror.
    pub fn mirror(&self) -> &crate::mirror::Mirror {
        &self.mirror
    }
}

impl Drop for MirrorHandle {
    fn drop(&mut self) {
        self.mirror.unwatch();
    }
}

/// The base tree's files, read from the fresh workspace (at the base commit, before the agent runs): every
/// file but `.git`, mode `100755` when executable (unix), else `100644`.
pub fn base_files(dir: &Path) -> Vec<crate::mirror::BaseFile> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<crate::mirror::BaseFile>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if e.file_name() == ".git" {
                continue;
            }
            if ft.is_dir() {
                walk(root, &p, out);
            } else if ft.is_file() {
                let Ok(rel) = p.strip_prefix(root) else {
                    continue;
                };
                let rel = rel.to_string_lossy().replace('\\', "/");
                let Ok(bytes) = std::fs::read(&p) else {
                    continue;
                };
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    match e.metadata() {
                        Ok(m) if m.permissions().mode() & 0o111 != 0 => "100755",
                        _ => "100644",
                    }
                };
                #[cfg(not(unix))]
                let mode = "100644";
                out.push(crate::mirror::BaseFile {
                    path: rel,
                    mode: mode.into(),
                    bytes,
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Starts mirroring the worktree into han-timeline: every batch is published on the session's stream as a
/// `han.timeline/1` item (masked, SR-52), attributed to the running tool call (TL-91).
#[allow(clippy::too_many_arguments)]
pub fn start_mirror(
    api: Arc<dyn han_node::api::NodeApi>,
    space: &str,
    session: &str,
    store_dir: &Path,
    workspace: &Workspace,
    signer: han_timeline::KeySigner,
    masker: Arc<crate::mask::Masker>,
    base: Vec<crate::mirror::BaseFile>,
) -> Result<Arc<MirrorHandle>, String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::mirror::MirrorBatch>();
    let mirror = crate::mirror::Mirror::open(
        crate::mirror::MirrorConfig {
            worktree: workspace.dir.clone(),
            store_dir: store_dir.to_path_buf(),
            space: space.to_owned(),
            session: session.to_owned(),
            signer,
            base_commit: workspace.base.clone(),
            base_files: base,
            masker,
            ignore: None,
            quiet: std::time::Duration::from_millis(200),
        },
        Box::new(move |b| {
            let _ = tx.send(b);
        }),
    )
    .map_err(|e| e.to_string())?;
    let (space, session) = (space.to_owned(), session.to_owned());
    tokio::spawn(async move {
        // In order, one at a time: the stream numbers items as they are sent.
        while let Some(b) = rx.recv().await {
            let r = api
                .call(han_node::api::Command::StreamPublish {
                    space: space.clone(),
                    session: session.clone(),
                    name: "han.timeline/1".into(),
                    body: b.body().to_string(),
                })
                .await;
            if let Err(e) = r {
                tracing::warn!("timeline batch not published: {e}");
            }
        }
    });
    mirror.watch().map_err(|e| e.to_string())?;
    Ok(Arc::new(MirrorHandle { mirror }))
}

#[cfg(test)]
mod tests {
    use super::adapter_read_root;
    use std::path::Path;

    #[test]
    fn the_adapter_reads_the_whole_node_modules_it_was_installed_into() {
        // `han agent setup-harness claude`: the adapter's dependencies (`@agentclientprotocol/sdk`, the Agent
        // SDK and its `claude` executable) are its siblings, not inside its package directory.
        let install = Path::new("/data/harness/claude");
        let adapter = install.join("node_modules/@agentclientprotocol/claude-agent-acp");
        assert_eq!(adapter_read_root(&adapter), install.join("node_modules"));
        let pi = install.join("node_modules/pi-acp");
        assert_eq!(adapter_read_root(&pi), install.join("node_modules"));
        // A nested install still resolves through the outer tree.
        let nested = install.join("node_modules/a/node_modules/b");
        assert_eq!(adapter_read_root(&nested), install.join("node_modules"));
        // An adapter checked out somewhere else keeps its own directory.
        let dev = Path::new("/src/my-adapter");
        assert_eq!(adapter_read_root(dev), dev);
    }
}
