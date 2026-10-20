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
use crate::host::{HostConfig, ProxyHook};
use crate::mask::Masker;
use crate::proxy::{
    AllowEntry, Budget, KeySource, KeyStyle, Ledger, ModelRoute, Price, Proxy, ProxyConfig,
    ProxyHandle, Roots,
};
use crate::sandbox::{self, Limits, Preference, ProxyTarget, SandboxSpec, Sandboxed};
use crate::stderr_log::{self, StderrLog};

/// The session's egress: its proxy and the token the agent presents.
pub struct Egress {
    proxy: ProxyHandle,
    token: String,
    session: String,
    budget: Option<SessionBudget>,
}

/// How many ledger units make one unit of the budget. Prices are per million tokens, so the ledger (which
/// prices a token in whole units) counts in millionths of the budget's unit and nothing rounds to zero.
pub const LEDGER_SCALE: u64 = 1_000_000;

/// The budget of a session as the host holds it.
#[derive(Clone)]
pub struct SessionBudget {
    /// The budget's unit.
    pub unit: String,
    /// The limit, in the unit.
    pub limit: u64,
    /// The ledger the proxy asks before each model call; `None` when the budget cannot be enforced.
    pub ledger: Option<Arc<Ledger>>,
    /// Why it cannot be enforced (`no price table`).
    pub not_enforced: Option<&'static str>,
}

impl SessionBudget {
    /// The budget of `spec`. It is enforced only on the model route (API-key mode) and with prices to
    /// reserve at: a subscription session has no per-call price, so nothing is reserved and the status says
    /// so.
    pub fn new(spec: &han_registry::BudgetSpec, model_route: bool) -> SessionBudget {
        let enforced = model_route && !spec.prices.is_empty();
        let ledger = enforced.then(|| {
            let prices = spec
                .prices
                .iter()
                .map(|(model, p)| {
                    (
                        model.clone(),
                        Price {
                            input: p.input,
                            output: p.output,
                            cache_read: p.cache_read,
                            cache_creation: p.cache_creation,
                        },
                    )
                })
                .collect();
            Arc::new(Ledger::new(spec.limit.saturating_mul(LEDGER_SCALE), prices))
        });
        SessionBudget {
            unit: spec.unit.clone(),
            limit: spec.limit,
            not_enforced: ledger.is_none().then_some("no price table"),
            ledger,
        }
    }

    /// What has been spent, in the unit (rounded up, so a started unit counts).
    pub fn spent(&self) -> u64 {
        self.ledger
            .as_ref()
            .map_or(0, |l| l.spent().div_ceil(LEDGER_SCALE))
    }
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

/// `path` without its `.` and `..` components (the files named need not exist yet to compare).
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The object stores a session's git needs read-only, at the same path inside the sandbox: the store the
/// workspace was made from and every object directory the worktree's `.git/objects/info/alternates` names
/// (and those directories' own alternates). The alternates file is what git reads, so it is the truth: a
/// worktree that a restarted host picks up again has no `shared_store`, and without the bind every git
/// command fails with `bad object HEAD`. The worktree's own `.git/objects` stays writable (it is under the
/// worktree), so commits land there, and the shared store is never written by the agent.
pub fn store_binds(workspace: &Workspace) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if p.exists() && !p.starts_with(&workspace.dir) && !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(store) = &workspace.shared_store {
        add(store.clone());
    }
    let mut pending = vec![workspace.dir.join(".git").join("objects")];
    let mut seen = 0;
    while let Some(objects) = pending.pop() {
        // a chain of alternates is short; a loop or a long one is not followed further
        seen += 1;
        if seen > 8 {
            break;
        }
        let Ok(text) = std::fs::read_to_string(objects.join("info").join("alternates")) else {
            continue;
        };
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let path = Path::new(line);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                objects.join(path)
            };
            let path = lexical(&path);
            pending.push(path.clone());
            add(path);
        }
    }
    out
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

/// The backend `pref` gets on this machine, as a preference that names it, or why the session cannot start
/// (SR-23): a named backend that does not work, MXC without a loopback port included, never becomes advisory.
async fn resolve(
    pref: Preference,
    allow_advisory: bool,
    shim: &sandbox::ShimCommand,
) -> Result<Preference, String> {
    let shim = shim.clone();
    let available = tokio::task::spawn_blocking(move || sandbox::probe(&shim))
        .await
        .map_err(|e| e.to_string())?;
    Ok(
        match sandbox::admit(pref, allow_advisory, &available).map_err(|e| e.to_string())? {
            SandboxBackend::Mxc => Preference::Mxc,
            SandboxBackend::Bwrap => Preference::Bwrap,
            SandboxBackend::None => Preference::None,
        },
    )
}

impl Egress {
    /// Starts the session's proxy: the driver's CONNECT allow-list, the model route in API-key mode, and
    /// han-mcp on the tunnel.
    pub async fn start(
        cfg: &HostConfig,
        hook: Option<&ProxyHook>,
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
        hosts.extend(crate::toolchain::allow_hosts(&cfg.sandbox.toolchains));
        pc.allow = hosts.into_iter().map(AllowEntry::https).collect();
        if let CredentialMode::ApiKey {
            provider_base,
            header,
            paths,
            ca_file,
            ..
        } = &cfg.driver.credentials
        {
            let roots =
                match ca_file {
                    Some(f) => Roots::Only(vec![std::fs::read(f).map_err(|e| {
                        format!("the model provider's CA file {}: {e}", f.display())
                    })?]),
                    None => Roots::System,
                };
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
                roots,
            });
        }
        let b = Arc::clone(bridge);
        pc.mcp = Some(Arc::new(move |stream| {
            let b = Arc::clone(&b);
            tokio::spawn(async move {
                if let Err(e) = han_mcp::serve(stream, b).await {
                    tracing::debug!("han-mcp session ended: {e}");
                }
            });
        }));
        let budget = cfg
            .budget
            .as_ref()
            .map(|b| SessionBudget::new(b, pc.model.is_some()));
        if let Some(ledger) = budget.as_ref().and_then(|b| b.ledger.clone()) {
            pc.budget = Some(ledger as Arc<dyn Budget>);
        }
        if let Some(hook) = hook {
            hook(&mut pc);
        }
        let proxy = Proxy::start(pc).await.map_err(|e| e.to_string())?;
        Ok(Egress {
            proxy,
            token: token.to_owned(),
            session,
            budget,
        })
    }

    /// The session's budget, when the space has one.
    pub fn budget(&self) -> Option<&SessionBudget> {
        self.budget.as_ref()
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
        let backend = resolve(
            Preference::parse(&cfg.sandbox.backend)?,
            cfg.sandbox.allow_advisory,
            &cfg.shim,
        )
        .await?;
        // Inside a sandbox the agent reaches the proxy through the shim's loopback port; without one, the
        // proxy's own port.
        let inside_port = match backend {
            Preference::None => None,
            _ => Some(sandbox::pick_inside_port().map_err(|e| e.to_string())?),
        };
        let port = inside_port.unwrap_or_else(|| self.proxy.addr().port());
        let login = login_binding(&cfg.driver, cfg.sandbox.login_dir.as_deref())?;
        let volatile_dirs = cfg
            .driver
            .volatile_login_paths(login.as_ref().map(|(_, dir)| dir.as_path()))?;
        cfg.driver
            .check_configuration(&workspace.dir, login.as_ref().map(|(_, dir)| dir.as_path()))?;
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
        let mut ro: Vec<PathBuf> = vec![prepared.program.clone(), cfg.shim.program.clone()];
        if cfg.node.is_absolute() {
            ro.push(cfg.node.clone());
        }
        if !cfg.adapter_dir.as_os_str().is_empty() {
            ro.push(adapter_read_root(&cfg.adapter_dir));
        }
        ro.extend(store_binds(workspace));
        ro.extend(cfg.sandbox.ro.iter().cloned());
        let mut env = prepared.env.clone();
        let mut rw = vec![workspace.dir.clone(), agent_dir.to_path_buf(), temp.clone()];
        // Toolchains: read-only roots, per-agent caches and the environment that points at them. A missing
        // piece fails the session here, before anything runs.
        let tools = crate::toolchain::plan(
            &cfg.sandbox.toolchains,
            crate::toolchain::Target::of(backend),
            agent_dir.parent().unwrap_or(agent_dir),
            &workspace.dir,
        )
        .map_err(|e| format!("toolchain: {e}"))?;
        ro.extend(tools.ro.iter().cloned());
        rw.extend(tools.rw.iter().cloned());
        crate::toolchain::apply(&tools, &mut env);
        // Steps a backend cannot do inside (MXC cannot unpack downloaded packages): the host does them first.
        let mut tool_gaps = tools.gaps.clone();
        tool_gaps.extend(crate::toolchain::prewarm(&tools, self.proxy.addr(), &self.token).await);
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
            volatile_dirs,
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
            shim: cfg.shim.clone(),
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
        gaps.extend(tool_gaps);
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
    use super::{LEDGER_SCALE, SessionBudget, adapter_read_root};
    use crate::proxy::{Budget, CallBound, Usage};
    use han_registry::{BudgetSpec, PriceSpec};
    use std::path::Path;

    fn spec(prices: &[(&str, PriceSpec)]) -> BudgetSpec {
        BudgetSpec {
            unit: "USD-cent".into(),
            limit: 100,
            prices: prices.iter().map(|(m, p)| ((*m).to_owned(), *p)).collect(),
        }
    }

    const SONNET: PriceSpec = PriceSpec {
        input: 300,
        output: 1500,
        cache_read: 0,
        cache_creation: 0,
    };

    #[test]
    fn a_budget_is_enforced_only_on_the_model_route_with_prices() {
        let priced = spec(&[("claude-sonnet", SONNET)]);
        assert!(SessionBudget::new(&priced, true).ledger.is_some());
        // subscription mode: no model route, nothing to reserve, and it says so
        let sub = SessionBudget::new(&priced, false);
        assert!(sub.ledger.is_none());
        assert_eq!(sub.not_enforced, Some("no price table"));
        // no table for the unit
        let none = SessionBudget::new(&spec(&[]), true);
        assert!(none.ledger.is_none());
        assert_eq!(none.not_enforced, Some("no price table"));
    }

    #[test]
    fn spend_is_in_the_unit_and_a_spent_budget_refuses() {
        let b = SessionBudget::new(&spec(&[("claude-sonnet", SONNET)]), true);
        let ledger = b.ledger.clone().unwrap();
        let call = CallBound {
            model: Some("claude-sonnet-5-5".into()),
            input_tokens: 1_000,
            max_output: Some(1_000),
        };
        // worst case 1000 in at 300 + 1000 out at 1500 per million = 1.8 cents: it fits
        let hold = ledger.reserve(&call).unwrap();
        assert_eq!(hold.amount, 1_000 * 300 + 1_000 * 1_500);
        // it used 100 in and 100 out: 0.18 cents, rounded up to one
        ledger.settle(
            hold,
            &Usage {
                input: 100,
                output: 100,
                requests: 1,
                ..Usage::default()
            },
        );
        assert_eq!(b.spent(), 1);
        assert_eq!(ledger.spent(), 100 * 300 + 100 * 1_500);
        // a call that cannot fit what is left is refused
        let big = CallBound {
            max_output: Some(100 * LEDGER_SCALE / 1_500 * 2),
            ..call
        };
        assert!(ledger.reserve(&big).is_err());
    }

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
