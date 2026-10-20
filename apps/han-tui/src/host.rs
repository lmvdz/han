//! Carries out the app's [`Action`]s against a [`NodeApi`] and feeds what happens back as [`Msg`]s. It also
//! owns the connection: when the daemon goes away it drops every subscription and dials again (1 s, 2 s, 4 s,
//! then every 5 s) until the node answers, then tells the app, which opens its views afresh.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_node::NodeApi;
use han_node::api::{BoxFuture, NodeError, ViewEvent};
use han_node::rpc::RpcClient;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::app::App;
use crate::msg::{Action, Msg, ViewKind};

/// How the host reaches the node. A trait so that tests can hand out fakes and count the dials.
pub trait Connector: Send + Sync + 'static {
    /// Dials the node.
    fn connect(&self) -> BoxFuture<'static, io::Result<Arc<dyn NodeApi>>>;
}

/// Dials the daemon's pipe or socket.
pub struct RpcConnector {
    /// The endpoint (see `han_node::rpc::default_endpoint`).
    pub endpoint: String,
}

impl Connector for RpcConnector {
    fn connect(&self) -> BoxFuture<'static, io::Result<Arc<dyn NodeApi>>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let client = RpcClient::connect(&endpoint).await?;
            Ok(Arc::new(client) as Arc<dyn NodeApi>)
        })
    }
}

type Shared = Arc<Mutex<Option<Arc<dyn NodeApi>>>>;

/// The doer of actions.
pub struct Host {
    api: Shared,
    connector: Option<Arc<dyn Connector>>,
    tx: UnboundedSender<Msg>,
    subs: HashMap<ViewKind, JoinHandle<()>>,
    dialer: Option<JoinHandle<()>>,
    config_path: Option<PathBuf>,
}

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Option<Arc<dyn NodeApi>>> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

impl Host {
    fn new(
        api: Option<Arc<dyn NodeApi>>,
        connector: Option<Arc<dyn Connector>>,
        config_path: Option<PathBuf>,
        tx: UnboundedSender<Msg>,
    ) -> Host {
        Host {
            api: Arc::new(Mutex::new(api)),
            connector,
            tx,
            subs: HashMap::new(),
            dialer: None,
            config_path,
        }
    }

    /// Carries out actions. True when one of them was `Quit`.
    pub fn execute(&mut self, actions: Vec<Action>) -> bool {
        let mut quit = false;
        for a in actions {
            match a {
                Action::Subscribe { view, epoch, query } => {
                    if let Some(old) = self.subs.remove(&view) {
                        old.abort();
                    }
                    let Some(api) = lock(&self.api).clone() else {
                        continue;
                    };
                    let tx = self.tx.clone();
                    self.subs.insert(
                        view,
                        tokio::spawn(async move {
                            let mut stream = api.view(*query);
                            while let Some(event) = stream.next().await {
                                let failed = matches!(event, ViewEvent::Failed { .. });
                                if tx.send(Msg::View { view, epoch, event }).is_err() || failed {
                                    break;
                                }
                            }
                            let _ = tx.send(Msg::ViewEnded { view, epoch });
                        }),
                    );
                }
                Action::Unsubscribe(view) => {
                    if let Some(old) = self.subs.remove(&view) {
                        old.abort();
                    }
                }
                Action::Call { id, command } => {
                    let tx = self.tx.clone();
                    let api = lock(&self.api).clone();
                    tokio::spawn(async move {
                        let result = match api {
                            Some(api) => api.call(command).await,
                            None => Err(NodeError::unavailable("not connected to the node")),
                        };
                        let _ = tx.send(Msg::Called { id, result });
                    });
                }
                Action::SaveConfig(config) => {
                    if let Some(path) = &self.config_path {
                        // A config that cannot be written costs only the memory of a choice.
                        let _ = config.save(path);
                    }
                }
                Action::SetupCall {
                    id,
                    command,
                    config,
                } => {
                    let saved = self
                        .config_path
                        .as_ref()
                        .ok_or_else(|| std::io::Error::other("setup needs a preferences file"))
                        .and_then(|path| config.save(path));
                    if let Err(error) = saved {
                        let _ = self.tx.send(Msg::Called {
                            id,
                            result: Err(NodeError::invalid(format!(
                                "Setup was not sent: cannot save recovery information: {error}"
                            ))),
                        });
                        continue;
                    }
                    let tx = self.tx.clone();
                    let api = lock(&self.api).clone();
                    tokio::spawn(async move {
                        let result = match api {
                            Some(api) => api.call(command).await,
                            None => Err(NodeError::unavailable("not connected to the node")),
                        };
                        let _ = tx.send(Msg::Called { id, result });
                    });
                }
                Action::Reconnect => self.reconnect(),
                Action::Quit => quit = true,
            }
        }
        quit
    }

    /// Drops the connection and dials until it works.
    fn reconnect(&mut self) {
        for (_, h) in self.subs.drain() {
            h.abort();
        }
        *lock(&self.api) = None;
        if let Some(d) = self.dialer.take() {
            d.abort();
        }
        let Some(connector) = self.connector.clone() else {
            return;
        };
        let (tx, shared) = (self.tx.clone(), Arc::clone(&self.api));
        self.dialer = Some(tokio::spawn(async move {
            let mut attempt = 0u32;
            loop {
                attempt += 1;
                let _ = tx.send(Msg::Connecting { attempt });
                if let Ok(api) = connector.connect().await {
                    *lock(&shared) = Some(api);
                    let _ = tx.send(Msg::Connected);
                    return;
                }
                let wait = (1u64 << (attempt - 1).min(2)).min(5);
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        }));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for (_, h) in self.subs.drain() {
            h.abort();
        }
        if let Some(d) = self.dialer.take() {
            d.abort();
        }
    }
}

/// The app, its host and the channel between them and the world.
pub struct Runtime {
    /// The state.
    pub app: App,
    host: Host,
    rx: UnboundedReceiver<Msg>,
    tx: UnboundedSender<Msg>,
}

impl Runtime {
    /// A runtime. With `api` the connection is already there; without it `connector` is dialed (the TUI may
    /// start before the daemon does).
    pub fn new(
        app: App,
        api: Option<Arc<dyn NodeApi>>,
        connector: Option<Arc<dyn Connector>>,
        config_path: Option<PathBuf>,
    ) -> Runtime {
        let (tx, rx) = unbounded_channel();
        let had_api = api.is_some();
        let mut host = Host::new(api, connector, config_path, tx.clone());
        if had_api {
            let _ = tx.send(Msg::Connected);
        } else {
            host.reconnect();
        }
        Runtime { app, host, rx, tx }
    }

    /// A way to send messages in (the terminal's events, the clock).
    pub fn sender(&self) -> UnboundedSender<Msg> {
        self.tx.clone()
    }

    /// Handles one message.
    pub fn handle(&mut self, msg: Msg) {
        let actions = self.app.update(msg);
        if self.host.execute(actions) {
            self.app.quit = true;
        }
    }

    /// Waits for the next message.
    pub async fn recv(&mut self) -> Option<Msg> {
        self.rx.recv().await
    }

    /// Handles every message that is already there; how many there were.
    pub fn drain(&mut self) -> usize {
        let mut n = 0;
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
            n += 1;
        }
        n
    }
}
