//! Manual check of the platform's presence backend on a real device. It shows the availability (nothing on
//! screen) and, with `--prompt`, the system prompt for a made-up plan, and reports what the person did.
//!
//!     cargo run -p han-presence --example hello -- --prompt

use std::time::Duration;

use han_presence::{BackendChoice, PresencePrompt};

fn main() {
    #[cfg(windows)]
    println!(
        "Windows Hello availability: {:?}",
        han_presence::windows_hello::availability().map(|a| a.0)
    );
    if !std::env::args().any(|a| a == "--prompt") {
        println!("pass --prompt to show the system prompt");
        return;
    }
    let prompt = PresencePrompt {
        action: "git_push".into(),
        target: "github:example/repo".into(),
        plan_digest_short: PresencePrompt::short("0123456789abcdef0123456789abcdef"),
        space: "manual-check".into(),
        requester: "@agent:example".into(),
        timeout: Duration::from_secs(90),
    };
    println!("asking: {}", prompt.message());
    match BackendChoice::Auto.build().confirm(&prompt) {
        Ok(proof) => println!("confirmed by {}", proof.backend()),
        Err(e) => println!("not confirmed: {e}"),
    }
}
