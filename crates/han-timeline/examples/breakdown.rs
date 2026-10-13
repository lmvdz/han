//! Where a local op's time goes. `cargo run --release --example breakdown`.

use std::time::{Duration, Instant};

use han_timeline::envelope::{self, Compression};
use han_timeline::{
    At, Batch, History, HistoryKind, KeySigner, MemoryStore, ObjectId, Op, Signer as _,
};

fn main() {
    let a = KeySigner::from_seed("@alice:x.example", "dev", [7; 32]);
    let (mut h, g) = History::create(
        MemoryStore::default(),
        "s1:a.example",
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: "hello\n".into(),
            transcript: false,
        }],
    )
    .unwrap();
    let main = h.main().unwrap();
    let text = ObjectId::Op(g.ops[1].clone());
    let at = At::Timeline(main.clone());
    let (mut t_state, mut t_plan, mut t_write) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let mut s = 1u64;
    let n = 10_000;
    let mut last_batch: Option<Batch> = None;
    for _ in 0..n {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let t0 = Instant::now();
        let len = h.state(&at).unwrap().text(&text).unwrap().chars().count();
        let t1 = Instant::now();
        let op = h
            .plan_edit(&at, &text, (s >> 33) as usize % (len + 1), 0, "x")
            .unwrap()
            .unwrap();
        let t2 = Instant::now();
        let w = h.write(&a, 0, &main, vec![op], None).unwrap();
        let t3 = Instant::now();
        t_state += t1 - t0;
        t_plan += t2 - t1;
        t_write += t3 - t2;
        let env = han_timeline::Envelope::decode(&w.envelope).unwrap();
        last_batch = Some(env.batch().unwrap().0);
    }
    let per = |d: Duration| d.as_secs_f64() * 1e6 / n as f64;
    println!("state+text  {:8.1} µs", per(t_state));
    println!("plan_edit   {:8.1} µs", per(t_plan));
    println!("write       {:8.1} µs", per(t_write));

    // Pieces of a write, on the last batch.
    let b = last_batch.unwrap();
    let m = 10_000;
    let t = Instant::now();
    for _ in 0..m {
        std::hint::black_box(han_timeline::codec::encode_batch(&b));
    }
    println!(
        "encode      {:8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / m as f64
    );
    let raw = han_timeline::codec::encode_batch(&b);
    for c in [Compression::Zstd, Compression::Raw] {
        let t = Instant::now();
        for _ in 0..m {
            std::hint::black_box(envelope::compress(&raw, c));
        }
        println!(
            "compress {c:?} {:8.1} µs ({} bytes)",
            t.elapsed().as_secs_f64() * 1e6 / m as f64,
            raw.len()
        );
    }
    let frame = envelope::compress(&raw, Compression::Zstd);
    let t = Instant::now();
    for _ in 0..m {
        std::hint::black_box(envelope::decompress(&frame).unwrap());
    }
    println!(
        "decompress  {:8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / m as f64
    );
    let t = Instant::now();
    for _ in 0..m {
        std::hint::black_box(a.sign(b"han-timeline-sig-v1\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"));
    }
    println!(
        "sign        {:8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / m as f64
    );
    let env_bytes = envelope::seal("s1:a.example", Some(h.id()), &b, &a, Compression::Zstd);
    let env = han_timeline::Envelope::decode(&env_bytes).unwrap();
    let pk = a.public_key();
    let t = Instant::now();
    for _ in 0..m {
        std::hint::black_box(env.verify(&pk));
    }
    println!(
        "verify      {:8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / m as f64
    );
    let t = Instant::now();
    for _ in 0..m {
        std::hint::black_box(
            han_timeline::codec::decode_batch(&raw, a.actor(), a.key_id()).unwrap(),
        );
    }
    println!(
        "decode      {:8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / m as f64
    );
}
