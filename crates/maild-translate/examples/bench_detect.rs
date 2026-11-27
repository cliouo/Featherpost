//! Release-mode P95 latency benchmark for `maild_translate::detect_language` on 2,000-char samples.
//!
//! Run with:
//! `cargo run --release -p maild-translate --example bench_detect`

use std::time::Instant;

use maild_translate::{detect_language, MAX_SAMPLE_CHARS};

fn repeat_to_chars(seed: &str, target_chars: usize) -> String {
    let mut out = String::new();
    while out.chars().count() < target_chars {
        out.push_str(seed);
        out.push('\n');
    }
    out.chars().take(target_chars).collect()
}

fn main() {
    let samples = vec![
        (
            "en",
            repeat_to_chars(
                "Hello team, please review the quarterly engineering update and confirm whether all integration milestones are ready before Friday's deployment window. See https://example.com/report and contact dev@example.com for details.",
                MAX_SAMPLE_CHARS,
            ),
        ),
        (
            "zh-CN",
            repeat_to_chars(
                "请查收本周的研发进度汇总，我们在本次迭代中完成了翻译引擎的核心能力开发，并将在明天下午进行灰度验证与性能回归测试。",
                MAX_SAMPLE_CHARS,
            ),
        ),
        (
            "ja",
            repeat_to_chars(
                "いつもお世話になっております。次回のリリースに向けた検証結果とスケジュールを共有いたしますので、ご確認のほどよろしくお願いいたします。",
                MAX_SAMPLE_CHARS,
            ),
        ),
        (
            "de",
            repeat_to_chars(
                "Hallo zusammen, bitte prüfen Sie die aktualisierten Unterlagen für die morgige Besprechung und teilen Sie uns mit, ob alle offenen Punkte geklärt sind.",
                MAX_SAMPLE_CHARS,
            ),
        ),
        (
            "ru",
            repeat_to_chars(
                "Здравствуйте! Пожалуйста, ознакомьтесь с обновлённым отчётом по проекту и подтвердите готовность команды к выпуску новой версии на следующей неделе.",
                MAX_SAMPLE_CHARS,
            ),
        ),
        (
            "html-en",
            repeat_to_chars(
                "<div><p>Dear customer, your monthly statement is now available in your account portal. Please review all transactions and contact support if anything looks unexpected.</p></div>",
                MAX_SAMPLE_CHARS,
            ),
        ),
    ];

    // Warmup.
    for (_, text) in &samples {
        let _ = detect_language(text, Some("Weekly update"));
    }

    let iterations_per_sample = 500;
    let mut latencies_ns: Vec<u128> = Vec::with_capacity(samples.len() * iterations_per_sample);

    for _ in 0..iterations_per_sample {
        for (_, text) in &samples {
            let t0 = Instant::now();
            let res = detect_language(text, Some("Weekly update"));
            let elapsed = t0.elapsed().as_nanos();
            std::hint::black_box(res);
            latencies_ns.push(elapsed);
        }
    }

    latencies_ns.sort_unstable();
    let p50 = latencies_ns[latencies_ns.len() / 2];
    let p95 = latencies_ns[(latencies_ns.len() * 95) / 100];
    let p99 = latencies_ns[(latencies_ns.len() * 99) / 100];

    println!(
        "detect_language ({} runs on {}-char samples, text extraction/stripping included): p50={:.3}ms p95={:.3}ms p99={:.3}ms",
        latencies_ns.len(),
        MAX_SAMPLE_CHARS,
        p50 as f64 / 1_000_000.0,
        p95 as f64 / 1_000_000.0,
        p99 as f64 / 1_000_000.0,
    );

    assert!(
        p95 < 1_000_000,
        "Expected P95 < 1.0ms, got {:.3}ms",
        p95 as f64 / 1_000_000.0
    );
}
