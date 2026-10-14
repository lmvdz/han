//! `han seq`: run a home (the sequencer of one server) on a QUIC endpoint, with its well-known documents.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use han_core::types::Durability;
use han_net::wellknown::{document, serve_http, wellknown_routes};
use han_net::{
    HomeTicket, IrohOptions, IrohTransport, RelayChoice, ServeOptions, Transport, serve,
};
use han_seq::{Home, HomeConfig, HostingPolicy, SqliteStore, SystemClock};

use crate::keys;

/// Options of `han seq`.
#[derive(Args)]
pub struct SeqArgs {
    #[command(subcommand)]
    cmd: Option<SeqCmd>,
    /// The ServerName this home answers to (lowercase DNS name). Default: the one `han home init` recorded.
    #[arg(long)]
    server: Option<String>,
    /// The data directory (one SQLite file per space, plus `home.db` and the keys).
    #[arg(long, default_value = "han-data")]
    data: PathBuf,
    /// UDP address of the QUIC endpoint.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// TCP address of the HTTP server for `/.well-known/han` (put a TLS proxy in front of it).
    #[arg(long)]
    http: Option<SocketAddr>,
    /// Relays: `n0` (n0's public relays and DNS discovery, the default), `none`, or relay URLs separated by commas.
    #[arg(long, env = "HAN_RELAY")]
    relay: Option<String>,
    /// Create a profile space: `actor:kind:keyId:publicKey[:operator]`. Repeatable. Who may ask a server to
    /// register a profile is out of scope of the protocol (section 4.3).
    #[arg(long)]
    register: Vec<String>,
    /// Who may create a space on this server: `known` (its own actors and holders of a role in a hosted
    /// space; the default), `open`, or `allow:<actor-or-server>,...` (known actors and the listed ones).
    /// Test mode defaults to `open`.
    #[arg(long, value_parser = parse_hosting)]
    hosting: Option<HostingPolicy>,
    /// Test mode (G-11): the clock moves only by `test.clock`, the peer filter admits every node, and the
    /// actors of `--test-config` are provisioned. Never for production.
    #[arg(long)]
    test_mode: bool,
    /// `config.json` of the wire vectors, for `--test-mode`.
    #[arg(long, default_value = "conformance/wire-vectors/config.json")]
    test_config: PathBuf,
}

#[derive(Subcommand)]
enum SeqCmd {
    /// Alias of `han home register`. Creates a profile space on the running home at `--data`, with no restart. Admin only: it goes over the
    /// home's local admin socket, which only the OS user that runs `han seq` can reach. (Registration is out of
    /// scope of the protocol, section 4.3, so there is no wire call for it.)
    Register {
        /// The data directory of the running `han seq`.
        #[arg(long, default_value = "han-data")]
        data: PathBuf,
        /// The actor, `@name:server`.
        #[arg(long)]
        actor: String,
        /// `human`, `agent` or `service`.
        #[arg(long, default_value = "agent")]
        kind: String,
        /// The key's id.
        #[arg(long, default_value = "k1")]
        key_id: String,
        /// The key, base64url (`han agent create` prints it; it may start with `-`).
        #[arg(long, allow_hyphen_values = true)]
        public_key: String,
        /// The operator, for an agent or a service.
        #[arg(long)]
        operator: Option<String>,
    },
}

fn parse_hosting(spec: &str) -> Result<HostingPolicy, String> {
    match spec {
        "known" => Ok(HostingPolicy::KnownActors),
        "open" => Ok(HostingPolicy::Open),
        list => match list.strip_prefix("allow:") {
            Some(l) if !l.is_empty() => Ok(HostingPolicy::AllowList(
                l.split(',').map(str::to_owned).collect(),
            )),
            _ => Err("want known, open or allow:<actor-or-server>,...".into()),
        },
    }
}

/// Where a running home keeps its ticket, in its data directory.
pub const TICKET_FILE: &str = "home.ticket";

/// Options of `han ticket`.
#[derive(Args)]
pub struct TicketArgs {
    /// The data directory of the running `han seq`.
    #[arg(long, default_value = "han-data")]
    data: PathBuf,
}

/// `han ticket`: prints the home ticket `han seq` wrote, which teammates pass to `--home`.
pub fn ticket(args: &TicketArgs) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.data.join(TICKET_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: {e} (is `han seq` running with this --data?)",
            path.display()
        )
    })?;
    println!("{}", text.trim());
    Ok(())
}

/// Opens the home of `server` in `data`, with its receipt key (made on first use).
pub fn open_home(
    data: &std::path::Path,
    server: &str,
    store: Arc<SqliteStore>,
) -> Result<Arc<Home>, Box<dyn std::error::Error>> {
    let key = keys::signing_key(keys::load_or_create(&data.join("server.key"))?);
    Ok(Arc::new(Home::open(
        HomeConfig {
            server: server.to_owned(),
            key,
            key_id: "h1".into(),
            durability: Durability::Local,
        },
        Arc::new(SystemClock::new()),
        store,
    )?))
}

/// Writes the home ticket for `transport` to the data directory and returns its path. The ticket stands in for
/// the HTTPS well-known where a home has none (a placeholder ServerName).
pub fn write_ticket(
    data: &std::path::Path,
    home: &Home,
    transport: &IrohTransport,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut ticket_addr = transport.addr();
    if ticket_addr.addrs.is_empty() && ticket_addr.relays.is_empty() {
        ticket_addr = transport.loopback_addr();
    }
    let ticket = HomeTicket {
        server: home.server().to_owned(),
        node: ticket_addr,
        keys: home.server_keys(),
    }
    .to_text();
    let path = data.join(TICKET_FILE);
    std::fs::write(
        &path,
        format!(
            "{ticket}
"
        ),
    )?;
    Ok(path)
}

pub fn relay_choice(spec: &str) -> RelayChoice {
    match spec {
        "n0" => RelayChoice::N0,
        "none" => RelayChoice::Disabled,
        urls => RelayChoice::Custom(urls.split(',').map(str::to_owned).collect()),
    }
}

pub async fn run(args: SeqArgs) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(SeqCmd::Register {
        data,
        actor,
        kind,
        key_id,
        public_key,
        operator,
    }) = args.cmd
    {
        return crate::homecmd::register(
            &data,
            crate::seqadmin::Register {
                actor,
                kind,
                key_id,
                public_key,
                operator,
            },
        )
        .await;
    }
    let settings = crate::homecmd::HomeSettings::load(&args.data)?;
    let args = SeqArgs {
        server: args.server.or(settings.server),
        bind: args.bind.or(settings.bind),
        http: args.http.or(settings.http),
        relay: args.relay.or(settings.relay),
        hosting: args.hosting.or(settings
            .hosting
            .as_deref()
            .and_then(|h| parse_hosting(h).ok())),
        cmd: None,
        ..args
    };
    let http_addr = args
        .http
        .unwrap_or_else(|| "127.0.0.1:8787".parse().expect("a socket address"));
    let relay = args.relay.clone().unwrap_or_else(|| "n0".into());
    std::fs::create_dir_all(&args.data)?;
    let store = Arc::new(SqliteStore::open(&args.data)?);
    let home = if args.test_mode {
        let config = han_net::wirevec::TestConfig::load(&args.test_config)?;
        config.home(store)?
    } else {
        open_home(
            &args.data,
            args.server
                .as_deref()
                .ok_or("han seq needs --server (or `han home init --server …` first)")?,
            store,
        )?
    };
    if let Some(hosting) = args.hosting.clone() {
        home.set_hosting(hosting);
    }
    for spec in &args.register {
        let p: Vec<&str> = spec.split(':').collect();
        let [
            actor_name,
            actor_server,
            kind,
            key_id,
            public,
            operator @ ..,
        ] = &p[..]
        else {
            return Err(
                format!("--register {spec}: want actor:kind:keyId:publicKey[:operator]").into(),
            );
        };
        let actor = format!("{actor_name}:{actor_server}");
        let operator = (!operator.is_empty()).then(|| operator.join(":"));
        home.register(&actor, kind, key_id, public, operator.as_deref())?;
    }
    let seed = keys::load_or_create(&args.data.join("node.key"))?;
    let mut opts = IrohOptions::new(seed);
    opts.bind = args.bind;
    opts.relay = if args.test_mode && relay == "n0" {
        RelayChoice::Disabled
    } else {
        relay_choice(&relay)
    };
    let transport = IrohTransport::bind(opts).await?;
    transport.online(std::time::Duration::from_secs(5)).await;
    home.set_nodes(vec![transport.node_id()]);
    let _server = serve(
        transport.clone(),
        Arc::clone(&home),
        ServeOptions::default(),
    );
    write_ticket(&args.data, &home, &transport)?;
    let (doc_home, doc_transport) = (Arc::clone(&home), transport.clone());
    let routes = wellknown_routes(move || {
        let mut addr = doc_transport.addr();
        if addr.addrs.is_empty() && addr.relays.is_empty() {
            addr = doc_transport.loopback_addr();
        }
        document(&doc_home, &[addr])
    });
    let http = serve_http(http_addr, routes).await?;
    // `han seq register` reaches the running home here, so a new agent's profile needs no restart.
    let _admin = crate::seqadmin::serve(Arc::clone(&home), &crate::seqadmin::endpoint(&args.data))?;
    println!("server {}", home.server());
    println!("node {}", transport.node_id());
    let mut addrs = transport.addr().addrs;
    addrs.extend(transport.loopback_addr().addrs);
    addrs.sort();
    addrs.dedup();
    for a in addrs {
        println!("addr {a}");
    }
    for r in &transport.addr().relays {
        println!("relay {r}");
    }
    println!("http {}", http.local_addr());
    // Not the ticket itself: stdout may end up in a journal. `han ticket --data` shows it.
    println!("ticket-file {}", args.data.join(TICKET_FILE).display());
    tokio::signal::ctrl_c().await?;
    transport.shutdown().await;
    Ok(())
}
