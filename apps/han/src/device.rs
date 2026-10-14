//! `han device`: add a device to a person's profile, through the running node (a face over its pipe).
//!
//! * On the new device: `han device offer` prints the pairing code, the check to compare on both screens, and
//!   the key id the new device's `han node` must run with.
//! * On a device already in the profile: `han device pair <code>` adds the new key (a `key.add`).
//! * `han device revoke <keyId>` revokes another device's key.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command};
use han_node::rpc::{RpcClient, default_endpoint};

/// Options of `han device`.
#[derive(Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    cmd: DeviceCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// On the new device: prints the code an existing device pairs.
    Offer,
    /// On an existing device: adds the device the code names to this person's profile.
    Pair {
        /// The code `han device offer` printed (`hand1-…`).
        code: String,
    },
    /// Revokes a device's key.
    Revoke {
        /// The key id.
        key_id: String,
    },
}

/// The key id a paired device gets: `d` and the first 8 hex digits of the SHA-256 of its public key (the same
/// rule as the node's `DevicePair`).
pub fn paired_key_id(public_key: &str) -> String {
    format!(
        "d{}",
        &han_core::ids::sha256_hex(public_key.as_bytes())[..8]
    )
}

pub async fn run(args: DeviceArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        DeviceCmd::Offer => match client.call(Command::DeviceOffer).await? {
            CallResult::DeviceCode {
                code,
                node_id,
                check,
            } => {
                println!("code {code}");
                println!("check {check}");
                println!("key-id {}", paired_key_id(&node_id));
                println!(
                    "pair it from a device already in the profile (han device pair <code>), then run this node with --key-id {}",
                    paired_key_id(&node_id)
                );
            }
            other => return Err(format!("unexpected answer: {other:?}").into()),
        },
        DeviceCmd::Pair { code } => {
            let public = code.strip_prefix("hand1-").unwrap_or_default();
            client
                .call(Command::DevicePair { code: code.clone() })
                .await?;
            let digest = han_core::ids::sha256_hex(public.as_bytes());
            println!(
                "paired key {} (check {}-{})",
                paired_key_id(public),
                &digest[..4],
                &digest[4..8]
            );
        }
        DeviceCmd::Revoke { key_id } => {
            client
                .call(Command::DeviceRevoke {
                    key_id: key_id.clone(),
                })
                .await?;
            println!("revoked {key_id}");
        }
    }
    Ok(())
}
