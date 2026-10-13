//! `han dial`: a small client of a home, for scripts and for trying the protocol by hand.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use han_core::json::{self, Value};
use han_core::{SigningKey, base64url, ids};
use han_net::client::args as call_args;
use han_net::dial::{trusted_nodes_for_server, trusted_nodes_for_ticket};
use han_net::quic::IrohConn;
use han_net::wellknown::Fetcher;
use han_net::{Client, InviteTicket, IrohOptions, IrohTransport, NodeAddr};
use han_seq::build::{EntrySpec, sign_entry, space_policy};

use crate::keys;
use crate::seq::relay_choice;

type Error = Box<dyn std::error::Error>;

/// Options of `han dial`.
#[derive(Args)]
pub struct DialArgs {
    /// The key file of this device: its public key is its node id and signs its entries (N-01).
    #[arg(long, default_value = "device.key")]
    key_file: PathBuf,
    /// The actor to say hello as.
    #[arg(long)]
    actor: Option<String>,
    /// The id of that actor's key in its profile.
    #[arg(long, default_value = "d1")]
    key_id: String,
    /// The ServerName to dial (`dialed`, G-08). Its nodes come from `/.well-known/han` unless `--node` is given.
    #[arg(long)]
    server: Option<String>,
    /// Dial this node id directly (with `--addr` or `--relay`).
    #[arg(long)]
    node: Option<String>,
    /// A direct address of the node.
    #[arg(long)]
    addr: Vec<SocketAddr>,
    /// A relay URL of the node.
    #[arg(long)]
    relay_url: Vec<String>,
    /// Read the well-known documents of `--server` from this base URL instead of `https://<server>`.
    #[arg(long)]
    wellknown_base: Option<String>,
    /// This device's relays: `n0`, `none`, or URLs separated by commas.
    #[arg(long, default_value = "n0")]
    relay: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Say hello and print the answer.
    Hello,
    /// Make any call: `call <name> '<json args>'`.
    Call { name: String, args: String },
    /// Create a space (an owner is the creator).
    Create {
        space: String,
        #[arg(long, default_value = "members")]
        visibility: String,
        #[arg(long, default_value = "invite")]
        join: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Register an invite at the home and print the ticket text.
    Invite {
        space: String,
        /// Seconds the ticket lives (at most 7 days).
        #[arg(long, default_value_t = 86_400)]
        ttl: i64,
    },
    /// Use a ticket: dial its node, say hello, and join its space.
    Join { ticket: String },
    /// Give an actor a role in a space.
    MemberSet {
        space: String,
        actor: String,
        role: String,
    },
    /// Append an entry: `append <space> <kind> --body '<json>' [--content '<json>']`.
    Append {
        space: String,
        kind: String,
        #[arg(long, default_value = "{}")]
        body: String,
        #[arg(long)]
        content: Option<String>,
    },
    /// Read a space from `after`, as JSON lines; with `--follow` keep going.
    Read {
        space: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long)]
        follow: bool,
        /// Check the receipts against the home's published keys.
        #[arg(long)]
        verify: bool,
    },
    /// Print the first page of a space's state.
    State { space: String },
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn text(v: &Value) -> String {
    json::canonicalize(v)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

fn parse(arg: &str) -> Result<Value, Error> {
    Ok(json::parse(arg.as_bytes())?)
}

fn s(t: &str) -> Value {
    Value::Str(t.to_owned())
}

struct Session {
    client: Client<IrohConn>,
    actor: String,
    key: SigningKey,
    key_id: String,
    server: String,
    fetcher: Fetcher,
    node: NodeAddr,
}

pub async fn run(args: DialArgs) -> Result<(), Error> {
    let seed = keys::load_or_create(&args.key_file)?;
    let key = keys::signing_key(seed);
    let mut opts = IrohOptions::new(seed);
    opts.relay = relay_choice(&args.relay);
    let transport = IrohTransport::bind(opts).await?;
    let mut fetcher = Fetcher::new();
    if let (Some(base), Some(server)) = (&args.wellknown_base, &args.server) {
        fetcher = fetcher.with_base(server, base);
    }
    // where to dial, as which ServerName, and which node ids may answer
    let (server, node, trusted) = if let Command::Join { ticket } = &args.command {
        let t = InviteTicket::parse(ticket).ok_or("not a ticket")?;
        let trusted = trusted_nodes_for_ticket(&fetcher, &t).await?;
        (t.server, t.node, trusted)
    } else if let Some(node) = &args.node {
        let server = args
            .server
            .clone()
            .ok_or("--server is required: it is the ServerName in the hello proof")?;
        let addr = NodeAddr {
            node: node.clone(),
            addrs: args.addr.iter().map(ToString::to_string).collect(),
            relays: args.relay_url.clone(),
        };
        (server, addr, vec![node.clone()])
    } else {
        let server = args
            .server
            .clone()
            .ok_or("give --server (and --node), or a ticket")?;
        let (nodes, addrs) = trusted_nodes_for_server(&fetcher, &server).await?;
        (
            server,
            addrs
                .into_iter()
                .next()
                .ok_or("no node in the well-known document")?,
            nodes,
        )
    };
    let client = Client::connect(&transport, &node, &server, &trusted).await?;
    let actor = args.actor.clone().ok_or("--actor is required")?;
    client.hello(&actor, &args.key_id, &key).await?;
    let session = Session {
        client,
        actor,
        key,
        key_id: args.key_id,
        server,
        fetcher,
        node,
    };
    let done = session.command(args.command).await;
    session.client.close();
    transport.shutdown().await;
    done
}

impl Session {
    fn entry(
        &self,
        space: &str,
        kind: &str,
        body: Value,
        content: Option<Value>,
        seen: u64,
    ) -> Result<(Value, Option<Value>), Error> {
        Ok(sign_entry(
            EntrySpec {
                space: space.into(),
                kind: kind.into(),
                author: self.actor.clone(),
                key_id: self.key_id.clone(),
                body,
                content,
                refs: vec![],
                seen,
                time_secs: now_secs(),
            },
            &self.key,
        )?)
    }

    async fn head(&self, space: &str) -> Result<u64, Error> {
        let st = self
            .client
            .call("state", call_args([("space", s(space))]))
            .await?;
        Ok(st["state"]["head"].as_u64().unwrap_or(0))
    }

    async fn append(
        &self,
        space: &str,
        kind: &str,
        body: Value,
        content: Option<Value>,
    ) -> Result<(), Error> {
        let seen = if kind == "space.create" {
            0
        } else {
            self.head(space).await?
        };
        let (entry, content) = self.entry(space, kind, body, content, seen)?;
        let receipt = self.client.append(&entry, content.as_ref()).await?;
        println!("{}", text(&json::to_value(&receipt)?));
        Ok(())
    }

    async fn command(&self, command: Command) -> Result<(), Error> {
        match command {
            Command::Hello => println!("hello ok as {}", self.actor),
            Command::Call { name, args } => {
                let v = self.client.call(&name, parse(&args)?).await?;
                println!("{}", text(&v));
            }
            Command::Create {
                space,
                visibility,
                join,
                name,
            } => {
                let content = name.map(|n| Value::Object([("name".to_owned(), s(&n))].into()));
                let body =
                    Value::Object([("policy".to_owned(), space_policy(&visibility, &join))].into());
                self.append(
                    &space,
                    "space.create",
                    body,
                    content
                        .or_else(|| Some(Value::Object([("name".to_owned(), s(&space))].into()))),
                )
                .await?;
            }
            Command::Invite { space, ttl } => {
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(|e| e.to_string())?;
                let expires = han_seq::time::format_time(now_secs() + ttl);
                self.client
                    .call(
                        "invite",
                        call_args([
                            ("space", s(&space)),
                            ("hash", s(&ids::sha256_hex(&secret))),
                            ("expiresAt", s(&expires)),
                        ]),
                    )
                    .await?;
                let ticket = InviteTicket {
                    server: self.server.clone(),
                    node: self.node.clone(),
                    space,
                    secret: base64url::encode(&secret),
                };
                println!("{}", ticket.to_text());
            }
            Command::Join { ticket } => {
                let t = InviteTicket::parse(&ticket).ok_or("not a ticket")?;
                self.client
                    .call(
                        "join",
                        call_args([("space", s(&t.space)), ("secret", s(&t.secret))]),
                    )
                    .await?;
                println!("joined {}", t.space);
            }
            Command::MemberSet { space, actor, role } => {
                let body = Value::Object(
                    [
                        ("actor".to_owned(), s(&actor)),
                        ("role".to_owned(), s(&role)),
                    ]
                    .into(),
                );
                self.append(&space, "member.set", body, None).await?;
            }
            Command::Append {
                space,
                kind,
                body,
                content,
            } => {
                let content = content.as_deref().map(parse).transpose()?;
                self.append(&space, &kind, parse(&body)?, content).await?;
            }
            Command::Read {
                space,
                after,
                follow,
                verify,
            } => self.read(&space, after, follow, verify).await?,
            Command::State { space } => {
                let st = self
                    .client
                    .call("state", call_args([("space", s(&space))]))
                    .await?;
                println!("{}", text(&st));
            }
        }
        Ok(())
    }

    async fn read(&self, space: &str, after: u64, follow: bool, verify: bool) -> Result<(), Error> {
        let keys = if verify {
            Some(self.fetcher.wellknown(&self.server).await?.keys)
        } else {
            None
        };
        let mut stream = if follow {
            self.client.follow(space, after).await?
        } else {
            let items = self.client.read(space, after).await?;
            if let Some(keys) = &keys {
                let records: Vec<han_core::LogRecord> = items
                    .iter()
                    .filter(|i| i["type"].as_str() == Some("record"))
                    .map(|i| json::from_value(&i["record"]))
                    .collect::<Result<_, _>>()?;
                let first_prev = match records.first() {
                    Some(r) if r.receipt.seq > 1 => {
                        return Err("--verify needs the chain from seq 1".into());
                    }
                    _ => "",
                };
                han_seq::verify::check_chain(&records, keys, first_prev)?;
            }
            for item in &items {
                println!("{}", text(item));
            }
            return Ok(());
        };
        while let Some(item) = stream.next().await {
            println!("{}", text(&item?));
        }
        Ok(())
    }
}
