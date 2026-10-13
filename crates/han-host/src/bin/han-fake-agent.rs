//! `han-fake-agent`: a deterministic ACP agent for tests (PLAN §6.4 "Agent tests").
//!
//! ```text
//! han-fake-agent replay <script.json> [--log <file>]   replay a recorded session
//! han-fake-agent edge <case> [--log <file>]            a scripted edge case (see `edge_script`)
//! han-fake-agent mcp-echo                               a tiny MCP stdio server with a `propose` tool
//! ```
//!
//! A script is `{agent: {name, version}, modes?: {current, available}, mcpHttp?, turns: [{steps: [...]}]}`;
//! prompt N runs turn N (past the end: an empty `end_turn`). Steps (`type`):
//! `message {text}`, `thought {text}`, `tool_call {id, title, kind?, status?, rawInput?, locations?}`,
//! `tool_update {id, status?, title?, output?, rawOutput?, meta?}`, `write_file {path, content}`,
//! `edit_file {path, find, replace}`, `delete_file {path}` (relative paths, done for real in the cwd),
//! `plan {entries: [{content, status}]}`, `usage {used, size, cost?}`, `request_permission {id, title, kind?,
//! rawInput?, locations?, on_allow?: [steps], on_reject?: [steps]}`, `mcp_call {id, server, tool, args}`
//! (calls the tool on the stdio MCP server of that name from `session/new.mcpServers`), `sleep {ms}`,
//! `raw {line}` (writes a raw line to stdout), `exit {code}`, `end {stopReason?, meta?}`.
//!
//! With `--log` (or `HAN_FAKE_AGENT_LOG`) every request the agent receives is appended as one JSON line, so
//! tests can assert what the client sent (prompts, mcpServers, modes, permission outcomes).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, RequestPermissionRequest,
    SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
};
use agent_client_protocol::{Client, ConnectionTo, Lines};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SESSION: &str = "fake-session-1";

#[derive(Clone, Debug, Deserialize)]
struct Script {
    #[serde(default)]
    agent: AgentInfo,
    #[serde(default)]
    modes: Option<Modes>,
    #[serde(default, rename = "mcpHttp")]
    mcp_http: bool,
    #[serde(default)]
    turns: Vec<Turn>,
}

#[derive(Clone, Debug, Deserialize)]
struct AgentInfo {
    name: String,
    version: String,
}

impl Default for AgentInfo {
    fn default() -> Self {
        AgentInfo {
            name: "han-fake-agent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Modes {
    current: String,
    available: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Turn {
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Step {
    Message {
        text: String,
    },
    Thought {
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default, rename = "rawInput")]
        raw_input: Option<Value>,
        #[serde(default)]
        locations: Vec<String>,
    },
    ToolUpdate {
        id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        output: Option<String>,
        #[serde(default, rename = "rawOutput")]
        raw_output: Option<Value>,
        #[serde(default)]
        meta: Option<Value>,
    },
    WriteFile {
        path: String,
        content: String,
    },
    EditFile {
        path: String,
        find: String,
        replace: String,
    },
    DeleteFile {
        path: String,
    },
    Plan {
        entries: Vec<Value>,
    },
    Usage {
        used: u64,
        size: u64,
        #[serde(default)]
        cost: Option<Value>,
    },
    RequestPermission {
        id: String,
        title: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default, rename = "rawInput")]
        raw_input: Option<Value>,
        #[serde(default)]
        locations: Vec<String>,
        #[serde(default)]
        on_allow: Vec<Step>,
        #[serde(default)]
        on_reject: Vec<Step>,
    },
    McpCall {
        id: String,
        server: String,
        tool: String,
        #[serde(default)]
        args: Value,
    },
    Sleep {
        ms: u64,
    },
    Raw {
        line: String,
    },
    WriteOutside,
    Exit {
        code: i32,
    },
    End {
        #[serde(default, rename = "stopReason")]
        stop_reason: Option<String>,
        #[serde(default)]
        meta: Option<Value>,
    },
}

type Out = Arc<tokio::sync::Mutex<tokio::io::Stdout>>;

struct McpClient {
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

struct State {
    script: Script,
    turn: AtomicUsize,
    cwd: Mutex<PathBuf>,
    mcp_servers: Mutex<Vec<Value>>,
    mcp: tokio::sync::Mutex<HashMap<String, McpClient>>,
    log: Option<Mutex<std::fs::File>>,
    cancelled: AtomicBool,
    out: Out,
}

impl State {
    fn log(&self, v: Value) {
        if let Some(f) = &self.log {
            let mut f = f.lock().unwrap_or_else(|p| p.into_inner());
            let _ = writeln!(f, "{v}");
            let _ = f.flush();
        }
    }
}

fn update(cx: &ConnectionTo<Client>, u: Value) {
    if let Ok(n) =
        serde_json::from_value::<SessionNotification>(json!({"sessionId": SESSION, "update": u}))
        && let Err(e) = cx.send_notification(n)
    {
        eprintln!("han-fake-agent: notification not sent: {e}");
    }
}

fn rel_path(cwd: &Path, p: &str) -> Option<PathBuf> {
    let path = Path::new(p);
    if path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(cwd.join(path))
}

fn locs(ls: &[String]) -> Value {
    json!(ls.iter().map(|l| json!({"path": l})).collect::<Vec<_>>())
}

/// Runs steps; returns `Some((stop_reason, meta))` when an `end` step (or a cancel) ends the turn.
fn run_steps<'a>(
    st: &'a Arc<State>,
    cx: &'a ConnectionTo<Client>,
    steps: &'a [Step],
) -> futures::future::BoxFuture<'a, Option<(String, Option<Value>)>> {
    Box::pin(async move {
        for step in steps {
            if st.cancelled.load(Ordering::SeqCst) {
                return Some(("cancelled".into(), None));
            }
            let cwd = st.cwd.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match step {
                Step::Message { text } => update(
                    cx,
                    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}),
                ),
                Step::Thought { text } => update(
                    cx,
                    json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": text}}),
                ),
                Step::ToolCall {
                    id,
                    title,
                    kind,
                    status,
                    raw_input,
                    locations,
                } => {
                    let mut u = json!({"sessionUpdate": "tool_call", "toolCallId": id, "title": title,
                        "status": status.clone().unwrap_or_else(|| "pending".into()), "locations": locs(locations)});
                    if let Some(k) = kind {
                        u["kind"] = json!(k);
                    }
                    if let Some(r) = raw_input {
                        u["rawInput"] = r.clone();
                    }
                    update(cx, u);
                }
                Step::ToolUpdate {
                    id,
                    status,
                    title,
                    output,
                    raw_output,
                    meta,
                } => {
                    let mut u = json!({"sessionUpdate": "tool_call_update", "toolCallId": id});
                    if let Some(s) = status {
                        u["status"] = json!(s);
                    }
                    if let Some(t) = title {
                        u["title"] = json!(t);
                    }
                    if let Some(o) = output {
                        u["content"] =
                            json!([{"type": "content", "content": {"type": "text", "text": o}}]);
                    }
                    if let Some(r) = raw_output {
                        u["rawOutput"] = r.clone();
                    }
                    if let Some(m) = meta {
                        u["_meta"] = m.clone();
                    }
                    update(cx, u);
                }
                Step::WriteFile { path, content } => {
                    let ok = rel_path(&cwd, path).is_some_and(|p| {
                        p.parent()
                            .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
                            && std::fs::write(&p, content).is_ok()
                    });
                    st.log(json!({"event": "write_file", "path": path, "ok": ok}));
                }
                Step::EditFile {
                    path,
                    find,
                    replace,
                } => {
                    let ok = rel_path(&cwd, path).is_some_and(|p| {
                        std::fs::read_to_string(&p).ok().is_some_and(|t| {
                            std::fs::write(&p, t.replacen(find, replace, 1)).is_ok()
                        })
                    });
                    st.log(json!({"event": "edit_file", "path": path, "ok": ok}));
                }
                Step::DeleteFile { path } => {
                    let ok = rel_path(&cwd, path).is_some_and(|p| std::fs::remove_file(p).is_ok());
                    st.log(json!({"event": "delete_file", "path": path, "ok": ok}));
                }
                Step::Plan { entries } => {
                    let entries: Vec<Value> = entries
                        .iter()
                        .map(|e| {
                            json!({"content": e["content"], "status": e.get("status").cloned().unwrap_or(json!("pending")),
                                "priority": e.get("priority").cloned().unwrap_or(json!("medium"))})
                        })
                        .collect();
                    update(cx, json!({"sessionUpdate": "plan", "entries": entries}));
                }
                Step::Usage { used, size, cost } => {
                    let mut u =
                        json!({"sessionUpdate": "usage_update", "used": used, "size": size});
                    if let Some(c) = cost {
                        u["cost"] = c.clone();
                    }
                    update(cx, u);
                }
                Step::RequestPermission {
                    id,
                    title,
                    kind,
                    raw_input,
                    locations,
                    on_allow,
                    on_reject,
                } => {
                    let mut tc = json!({"toolCallId": id, "title": title, "status": "pending", "locations": locs(locations)});
                    if let Some(k) = kind {
                        tc["kind"] = json!(k);
                    }
                    if let Some(r) = raw_input {
                        tc["rawInput"] = r.clone();
                    }
                    let req = json!({"sessionId": SESSION, "toolCall": tc, "options": [
                        {"optionId": "allow-once", "name": "Yes", "kind": "allow_once"},
                        {"optionId": "reject", "name": "No", "kind": "reject_once"}]});
                    let outcome = match serde_json::from_value::<RequestPermissionRequest>(req) {
                        Ok(r) => cx
                            .send_request(r)
                            .block_task()
                            .await
                            .map(|resp| serde_json::to_value(&resp).unwrap_or_default())
                            .unwrap_or_else(|e| json!({"error": e.to_string()})),
                        Err(e) => json!({"error": e.to_string()}),
                    };
                    st.log(json!({"event": "permission", "id": id, "response": outcome}));
                    let allowed = outcome["outcome"]["optionId"]
                        .as_str()
                        .is_some_and(|o| o.starts_with("allow"));
                    let branch = if allowed { on_allow } else { on_reject };
                    if let Some(end) = run_steps(st, cx, branch).await {
                        return Some(end);
                    }
                }
                Step::McpCall {
                    id,
                    server,
                    tool,
                    args,
                } => {
                    let title = format!("mcp__{server}__{tool}");
                    update(
                        cx,
                        json!({"sessionUpdate": "tool_call", "toolCallId": id, "title": title, "kind": "other",
                            "status": "pending", "rawInput": args}),
                    );
                    update(
                        cx,
                        json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "in_progress"}),
                    );
                    let result = mcp_call(st, server, tool, args).await;
                    st.log(json!({"event": "mcp_result", "id": id, "result": &result}));
                    match result {
                        Ok(text) => update(
                            cx,
                            json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed",
                                "content": [{"type": "content", "content": {"type": "text", "text": text}}]}),
                        ),
                        Err(e) => update(
                            cx,
                            json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed",
                                "content": [{"type": "content", "content": {"type": "text", "text": e}}]}),
                        ),
                    }
                }
                Step::Sleep { ms } => {
                    let mut left = *ms;
                    while left > 0 {
                        if st.cancelled.load(Ordering::SeqCst) {
                            return Some(("cancelled".into(), None));
                        }
                        let d = left.min(20);
                        tokio::time::sleep(Duration::from_millis(d)).await;
                        left -= d;
                    }
                }
                Step::Raw { line } => {
                    let mut out = st.out.lock().await;
                    let _ = out.write_all(line.as_bytes()).await;
                    let _ = out.write_all(b"\n").await;
                    let _ = out.flush().await;
                }
                Step::WriteOutside => {
                    let up = cwd
                        .join("..")
                        .join(format!("han-fake-outside-{}.txt", std::process::id()));
                    let tmp = std::env::temp_dir()
                        .join(format!("han-fake-outside-{}.txt", std::process::id()));
                    let r1 = std::fs::write(&up, "outside").is_ok();
                    let r2 = std::fs::write(&tmp, "outside").is_ok();
                    let _ = std::fs::remove_file(&up);
                    let _ = std::fs::remove_file(&tmp);
                    st.log(json!({"event": "write_outside", "parent": r1, "temp": r2}));
                    update(
                        cx,
                        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text",
                            "text": format!("write outside: parent={r1} temp={r2}")}}),
                    );
                }
                Step::Exit { code } => {
                    let _ = st.out.lock().await.flush().await;
                    std::process::exit(*code);
                }
                Step::End { stop_reason, meta } => {
                    return Some((
                        stop_reason.clone().unwrap_or_else(|| "end_turn".into()),
                        meta.clone(),
                    ));
                }
            }
        }
        None
    })
}

async fn mcp_call(
    st: &Arc<State>,
    server: &str,
    tool: &str,
    args: &Value,
) -> Result<String, String> {
    let mut clients = st.mcp.lock().await;
    if !clients.contains_key(server) {
        let entry = st
            .mcp_servers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|s| s["name"] == server)
            .cloned()
            .ok_or_else(|| format!("no MCP server named {server} in session/new"))?;
        let command = entry["command"].as_str().ok_or("not a stdio server")?;
        let mut cmd = tokio::process::Command::new(command);
        for a in entry["args"].as_array().into_iter().flatten() {
            cmd.arg(a.as_str().unwrap_or(""));
        }
        for e in entry["env"].as_array().into_iter().flatten() {
            cmd.env(
                e["name"].as_str().unwrap_or(""),
                e["value"].as_str().unwrap_or(""),
            );
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn {command}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
        let mut c = McpClient {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
        };
        rpc(
            &mut c,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "han-fake-agent", "version": env!("CARGO_PKG_VERSION")}}),
        )
        .await?;
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        c.stdin
            .write_all(format!("{note}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        clients.insert(server.to_owned(), c);
    }
    let c = clients.get_mut(server).ok_or("no client")?;
    let r = rpc(c, "tools/call", json!({"name": tool, "arguments": args})).await?;
    let text: String = r["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if r["isError"] == true {
        Err(text)
    } else {
        Ok(text)
    }
}

async fn rpc(c: &mut McpClient, method: &str, params: Value) -> Result<Value, String> {
    let id = c.next_id;
    c.next_id += 1;
    let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    c.stdin
        .write_all(format!("{msg}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    c.stdin.flush().await.map_err(|e| e.to_string())?;
    loop {
        let line = tokio::time::timeout(Duration::from_secs(30), c.stdout.next_line())
            .await
            .map_err(|_| "MCP server timed out".to_string())?
            .map_err(|e| e.to_string())?
            .ok_or("MCP server closed")?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["id"] == json!(id) {
            if let Some(e) = v.get("error") {
                return Err(e.to_string());
            }
            return Ok(v["result"].clone());
        }
        // A request or notification from the server: ignore (answer pings).
        if v.get("method").is_some() && v.get("id").is_some() {
            let reply = json!({"jsonrpc": "2.0", "id": v["id"], "result": {}});
            let _ = c.stdin.write_all(format!("{reply}\n").as_bytes()).await;
        }
    }
}

fn edge_script(case: &str) -> Option<Script> {
    let t = |steps: Value| -> Turn {
        serde_json::from_value(json!({"steps": steps})).unwrap_or_default()
    };
    let turns: Vec<Turn> = match case {
        // pi-acp hides provider errors: the turn ends end_turn with nothing in it.
        "hidden-error" => vec![t(json!([{"type": "end", "stopReason": "end_turn"}]))],
        "crash" => vec![t(
            json!([{"type": "message", "text": "starting"}, {"type": "exit", "code": 1}]),
        )],
        "huge-output" => {
            let chunk = "x".repeat(4096);
            let mut steps: Vec<Value> = (0..256)
                .map(|_| json!({"type": "message", "text": chunk}))
                .collect();
            steps.push(json!({"type": "end"}));
            vec![t(Value::Array(steps))]
        }
        "slow" => {
            let mut steps = Vec::new();
            for i in 0..5 {
                steps.push(json!({"type": "message", "text": format!("part {i} ")}));
                steps.push(json!({"type": "sleep", "ms": 100}));
            }
            steps.push(json!({"type": "end"}));
            vec![t(Value::Array(steps))]
        }
        "permission-storm" => {
            let mut steps: Vec<Value> = (0..50)
                .map(|i| {
                    json!({"type": "request_permission", "id": format!("p{i}"), "title": format!("rm -rf build{i}"),
                        "kind": "execute", "rawInput": {"command": format!("rm -rf build{i}")}})
                })
                .collect();
            steps.push(json!({"type": "message", "text": "storm over"}));
            steps.push(json!({"type": "end"}));
            vec![t(Value::Array(steps))]
        }
        "stuck-tool" => vec![t(json!([
            {"type": "tool_call", "id": "stuck", "title": "sleep 3600", "kind": "execute", "status": "in_progress"},
            {"type": "sleep", "ms": 3_600_000},
            {"type": "end"}
        ]))],
        "invalid-json" => vec![t(json!([
            {"type": "raw", "line": "this is not json {"},
            {"type": "message", "text": "still here"},
            {"type": "end"}
        ]))],
        "write-outside" => vec![t(json!([{"type": "write_outside"}, {"type": "end"}]))],
        "network-permission" => vec![t(json!([
            {"type": "tool_call", "id": "net1", "title": "curl -X POST https://example.com/hook", "kind": "fetch"},
            {"type": "request_permission", "id": "net1", "title": "curl -X POST https://example.com/hook", "kind": "fetch",
             "rawInput": {"command": "curl -X POST https://example.com/hook"},
             "on_allow": [{"type": "tool_update", "id": "net1", "status": "completed", "output": "ran"}],
             "on_reject": [{"type": "tool_update", "id": "net1", "status": "failed", "output": "User refused permission to run tool"}]},
            {"type": "end"}
        ]))],
        _ => return None,
    };
    Some(Script {
        agent: AgentInfo::default(),
        modes: Some(Modes {
            current: "acceptEdits".into(),
            available: vec!["default".into(), "acceptEdits".into()],
        }),
        mcp_http: false,
        turns,
    })
}

/// The `mcp-echo` server: initialize, tools/list (one `propose` tool), tools/call (echoes the proposal).
fn mcp_echo() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();
    let mut n = 0u64;
    loop {
        line.clear();
        if std::io::BufRead::read_line(&mut stdin.lock(), &mut line).unwrap_or(0) == 0 {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(id) = v.get("id").cloned() else {
            continue;
        };
        let result = match v["method"].as_str().unwrap_or("") {
            "initialize" => json!({"protocolVersion": v["params"]["protocolVersion"],
                "capabilities": {"tools": {}}, "serverInfo": {"name": "mcp-echo", "version": "0"}}),
            "tools/list" => {
                json!({"tools": [{"name": "propose", "description": "Propose an effect.",
                "inputSchema": {"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}}]})
            }
            "tools/call" => {
                n += 1;
                json!({"content": [{"type": "text", "text": format!(
                    "Proposed as effect E-{n} (status: waiting-approval): {}", v["params"]["arguments"]["summary"].as_str().unwrap_or(""))}]})
            }
            _ => {
                let e = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}});
                let _ = writeln!(stdout, "{e}");
                let _ = stdout.flush();
                continue;
            }
        };
        let _ = writeln!(
            stdout,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        );
        let _ = stdout.flush();
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: han-fake-agent replay <script.json> | edge <case> | mcp-echo  [--log <file>]"
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log_path = std::env::var_os("HAN_FAKE_AGENT_LOG").map(PathBuf::from);
    let mut pos = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--log" {
            log_path = it.next().map(PathBuf::from);
        } else {
            pos.push(a);
        }
    }
    let script = match pos.first().map(String::as_str) {
        Some("mcp-echo") => return mcp_echo(),
        Some("replay") => {
            let path = pos.get(1).unwrap_or_else(|| usage());
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("read {path}: {e}");
                std::process::exit(2)
            });
            serde_json::from_str::<Script>(&text).unwrap_or_else(|e| {
                eprintln!("script {path}: {e}");
                std::process::exit(2)
            })
        }
        Some("edge") => {
            edge_script(pos.get(1).map(String::as_str).unwrap_or("")).unwrap_or_else(|| usage())
        }
        _ => usage(),
    };
    let log = log_path.map(|p| {
        Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .expect("open log"),
        )
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(serve(script, log));
}

async fn serve(script: Script, log: Option<Mutex<std::fs::File>>) {
    let out: Out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let st = Arc::new(State {
        script,
        turn: AtomicUsize::new(0),
        cwd: Mutex::new(std::env::current_dir().unwrap_or_default()),
        mcp_servers: Mutex::new(Vec::new()),
        mcp: tokio::sync::Mutex::new(HashMap::new()),
        log,
        cancelled: AtomicBool::new(false),
        out: Arc::clone(&out),
    });
    let incoming = Box::pin(futures::stream::unfold(
        BufReader::new(tokio::io::stdin()).lines(),
        |mut lines| async move {
            match lines.next_line().await {
                Ok(Some(l)) => Some((Ok(l), lines)),
                Ok(None) => None,
                Err(e) => Some((Err(e), lines)),
            }
        },
    ));
    let outgoing = Box::pin(futures::sink::unfold(
        out,
        |out: Out, line: String| async move {
            {
                let mut o = out.lock().await;
                o.write_all(line.as_bytes()).await?;
                o.write_all(b"\n").await?;
                o.flush().await?;
            }
            Ok::<_, std::io::Error>(out)
        },
    ));
    let (s1, s2, s3, s4, s5) = (
        Arc::clone(&st),
        Arc::clone(&st),
        Arc::clone(&st),
        Arc::clone(&st),
        Arc::clone(&st),
    );
    let result = agent_client_protocol::Agent
        .builder()
        .name("han-fake-agent")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx| {
                s1.log(json!({"event": "initialize", "request": serde_json::to_value(&req).unwrap_or_default()}));
                let resp = json!({
                    "protocolVersion": 1,
                    "agentCapabilities": {"loadSession": false, "promptCapabilities": {},
                        "mcpCapabilities": {"http": s1.script.mcp_http, "sse": false}},
                    "agentInfo": {"name": s1.script.agent.name, "version": s1.script.agent.version},
                    "authMethods": []
                });
                responder.respond(serde_json::from_value::<InitializeResponse>(resp).map_err(internal)?)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, _cx| {
                let v = serde_json::to_value(&req).unwrap_or_default();
                s2.log(json!({"event": "new_session", "request": v}));
                if let Some(cwd) = v["cwd"].as_str() {
                    *s2.cwd.lock().unwrap_or_else(|p| p.into_inner()) = PathBuf::from(cwd);
                }
                *s2.mcp_servers.lock().unwrap_or_else(|p| p.into_inner()) =
                    v["mcpServers"].as_array().cloned().unwrap_or_default();
                let mut resp = json!({"sessionId": SESSION});
                if let Some(m) = &s2.script.modes {
                    resp["modes"] = json!({"currentModeId": m.current,
                        "availableModes": m.available.iter().map(|a| json!({"id": a, "name": a})).collect::<Vec<_>>()});
                }
                responder.respond(serde_json::from_value::<NewSessionResponse>(resp).map_err(internal)?)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest, responder, _cx| {
                s3.log(json!({"event": "set_mode", "request": serde_json::to_value(&req).unwrap_or_default()}));
                responder.respond(serde_json::from_value::<SetSessionModeResponse>(json!({})).map_err(internal)?)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let v = serde_json::to_value(&req).unwrap_or_default();
                s4.log(json!({"event": "prompt", "prompt": v["prompt"]}));
                s4.cancelled.store(false, Ordering::SeqCst);
                let n = s4.turn.fetch_add(1, Ordering::SeqCst);
                let st = Arc::clone(&s4);
                let cx2 = cx.clone();
                cx.spawn(async move {
                    let steps = st.script.turns.get(n).map(|t| t.steps.clone()).unwrap_or_default();
                    let end = run_steps(&st, &cx2, &steps).await;
                    let (stop, meta) = end.unwrap_or_else(|| {
                        if st.cancelled.load(Ordering::SeqCst) {
                            ("cancelled".into(), None)
                        } else {
                            ("end_turn".into(), None)
                        }
                    });
                    let mut resp = json!({"stopReason": stop});
                    if let Some(m) = meta {
                        resp["_meta"] = m;
                    }
                    responder.respond(serde_json::from_value::<PromptResponse>(resp).map_err(internal)?)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_n: CancelNotification, _cx| {
                s5.log(json!({"event": "cancel"}));
                s5.cancelled.store(true, Ordering::SeqCst);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Lines::new(outgoing, incoming))
        .await;
    if let Err(e) = result {
        eprintln!("han-fake-agent: {e}");
    }
}

fn internal(e: serde_json::Error) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(e.to_string())
}
