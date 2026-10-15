//! Measures the warm runner on this repo: the fast stage cold (empty target dir), then warm after a one-line change
//! in a leaf crate. Ignored: it builds the whole workspace.
//!
//! `cargo test -p han-exec --test warm_bench -- --ignored --nocapture`
//!
//! `HAN_WARM_BENCH_FILE` names another file to touch (default `integrations/han-github/src/main.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use han_exec::checks::Stage;
use han_exec::checks::warm::{Scope, WarmConfig, WarmEvidence, WarmRequest, WarmRunner};

fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=bench",
        "-c",
        "user.email=bench@example.invalid",
        "-C",
    ])
    .arg(dir)
    .args(args);
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    let out = cmd.output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn report(label: &str, ev: &WarmEvidence, wall_ms: u128) {
    println!(
        "{label}: ok={} scope={:?} wall={:.1}s",
        ev.ok,
        ev.scope,
        wall_ms as f64 / 1000.0
    );
    for s in &ev.steps {
        println!("  $ {}", s.command.join(" "));
        println!(
            "  {:<20} exit={:?} {:>8.1}s",
            s.name,
            s.exit,
            s.duration_ms as f64 / 1000.0
        );
    }
}

#[tokio::test]
#[ignore = "builds the whole workspace; run by hand for the numbers"]
async fn fast_stage_cold_then_warm_after_a_leaf_change() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    // HAN_WARM_BENCH_STATE keeps the runner's state (trees, targets, logs) between invocations, so a second
    // invocation measures the warm run alone
    let kept = std::env::var("HAN_WARM_BENCH_STATE")
        .ok()
        .map(PathBuf::from);
    let temp = tempfile::tempdir().unwrap();
    let state = kept.as_deref().unwrap_or(temp.path());
    let mut cfg = WarmConfig::new(state);
    cfg.step_timeout = std::time::Duration::from_secs(3600);
    cfg.run_timeout = std::time::Duration::from_secs(7200);
    // HAN_WARM_BENCH_ENV="NAME=value" adds a variable to the steps (for example CARGO_LOG, to see why cargo rebuilds)
    if let Ok(kv) = std::env::var("HAN_WARM_BENCH_ENV") {
        if let Some((k, v)) = kv.split_once('=') {
            cfg.extra_env.push((k.into(), v.into()));
        }
    }
    let runner = WarmRunner::new(cfg).unwrap();
    let base = git(&repo, &["rev-parse", "HEAD"], None);
    let req = |commit: &str, scope: Scope| WarmRequest {
        repo: repo.clone(),
        lane: "bench".into(),
        commit: commit.into(),
        scope,
        stage: Stage::Fast,
    };

    let t = Instant::now();
    let cold = runner.run(req(&base, Scope::Full)).await.unwrap();
    report("cold (full scope)", &cold, t.elapsed().as_millis());

    // a commit that adds one comment line to a leaf crate's file, made without touching any branch or the worktree
    let file = std::env::var("HAN_WARM_BENCH_FILE")
        .unwrap_or_else(|_| "integrations/han-github/src/main.rs".into());
    let old = git(&repo, &["show", &format!("{base}:{file}")], None);
    let tmp = tempfile::tempdir().unwrap();
    let index = tmp.path().join("index");
    let blob_src = tmp.path().join("blob");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::fs::write(
        &blob_src,
        format!(
            "{old}
// warm bench {stamp}
"
        ),
    )
    .unwrap();
    git(&repo, &["read-tree", &base], Some(&index));
    let blob = git(
        &repo,
        &["hash-object", "-w", &blob_src.to_string_lossy()],
        None,
    );
    git(
        &repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},{file}"),
        ],
        Some(&index),
    );
    let tree = git(&repo, &["write-tree"], Some(&index));
    let next = git(
        &repo,
        &["commit-tree", &tree, "-p", &base, "-m", "warm bench"],
        None,
    );

    let scope = runner
        .scope_for(&repo, "bench", &base, &next)
        .await
        .unwrap();
    println!("scope of the change: {scope:?}");
    let t = Instant::now();
    let warm = runner.run(req(&next, scope)).await.unwrap();
    report("warm (affected scope)", &warm, t.elapsed().as_millis());
    // what the warm run had to build again
    let rebuilt: Vec<String> = warm
        .steps
        .iter()
        .filter_map(|s| s.log.as_ref())
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .filter(|l| l.trim_start().starts_with("Compiling"))
        .map(|l| l.trim().to_string())
        .collect();
    println!("warm run compiled {} crates: {rebuilt:?}", rebuilt.len());
    assert!(cold.ok && warm.ok);
}
