//! The real terminal: raw mode on the alternate screen, key events from a thread, a clock tick, and the loop that
//! draws after every batch of messages.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use han_node::rpc::default_endpoint;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};

use crate::app::App;
use crate::config::Config;
use crate::host::{RpcConnector, Runtime};
use crate::msg::Msg;
use crate::ui;

/// What `han tui` was asked.
pub struct Options {
    /// The space to show (else the one last shown).
    pub space: Option<String>,
    /// The data directory of the running `han node` (names its pipe, and holds `tui.json`).
    pub data: PathBuf,
    /// The node's endpoint, if it is not the default of `data`.
    pub endpoint: Option<String>,
}

/// Runs the TUI until the user quits.
pub async fn run(opts: Options) -> io::Result<()> {
    let endpoint = opts
        .endpoint
        .unwrap_or_else(|| default_endpoint(&opts.data));
    let config_path = opts.data.join("tui.json");
    let config = Config::load(&config_path);
    let app = App::new(config, opts.space, Instant::now());
    let connector = Arc::new(RpcConnector { endpoint });
    let mut rt = Runtime::new(app, None, Some(connector), Some(config_path));

    let mut terminal = ratatui::try_init()?;
    // Pasted text arrives as one event, so a paste into the steering field is not a stream of key presses.
    let _ = ratatui::crossterm::execute!(io::stdout(), EnableBracketedPaste);
    let stop = Arc::new(AtomicBool::new(false));
    let reader = spawn_reader(rt.sender(), Arc::clone(&stop));
    let ticker = {
        let tx = rt.sender();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            loop {
                every.tick().await;
                if tx.send(Msg::Tick(Instant::now())).is_err() {
                    break;
                }
            }
        })
    };
    if let Ok((w, h)) = ratatui::crossterm::terminal::size() {
        rt.handle(Msg::Resize(w, h));
    }

    let result = loop {
        if let Err(e) = terminal.draw(|f| ui::draw(f, &rt.app)) {
            break Err(e);
        }
        let Some(msg) = rt.recv().await else {
            break Ok(());
        };
        rt.handle(msg);
        rt.drain();
        if rt.app.quit {
            break Ok(());
        }
    };
    stop.store(true, Ordering::Relaxed);
    ticker.abort();
    drop(rt);
    let _ = ratatui::crossterm::execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    let _ = reader.join();
    result
}

/// Reads terminal events on a thread of its own and forwards them. Key releases are dropped (Windows reports
/// both presses and releases).
fn spawn_reader(
    tx: tokio::sync::mpsc::UnboundedSender<Msg>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let msg = match event::read() {
                Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => Msg::Key(k),
                Ok(Event::Resize(w, h)) => Msg::Resize(w, h),
                Ok(Event::Paste(s)) => Msg::Paste(s),
                Ok(_) => continue,
                Err(_) => break,
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    })
}
