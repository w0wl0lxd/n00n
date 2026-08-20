use criterion::{Criterion, black_box, criterion_group, criterion_main};
use n00n_agent::diff::{DiffLine, DiffSpan, compute_hunks, unified_text};

fn generate_sample_text(lines: usize, change_frequency: usize) -> (String, String) {
    let mut before = String::new();
    let mut after = String::new();

    for i in 0..lines {
        let line = format!(
            "line_{i}: sample text for unified diff benchmarking string formatting performance\n"
        );
        if i % change_frequency == 0 {
            before.push_str("before_");
            before.push_str(&line);
            after.push_str("after_");
            after.push_str(&line);
        } else {
            before.push_str(&line);
            after.push_str(&line);
        }
    }

    (before, after)
}

fn render_hunks_string(
    summary: &str,
    display_path: &str,
    hunks: &[n00n_agent::diff::DiffHunk],
) -> String {
    let mut out = format!("{summary}\n--- {display_path}\n+++ {display_path}");
    let write_change = |out: &mut String, prefix: &str, spans: &[DiffSpan]| {
        out.push('\n');
        out.push_str(prefix);
        for s in spans {
            out.push_str(&s.text);
        }
    };
    for hunk in hunks {
        out.push('\n');
        for dl in &hunk.lines {
            match dl {
                DiffLine::Unchanged(t) => {
                    out.push_str("\n  ");
                    out.push_str(t);
                }
                DiffLine::Removed(spans) => write_change(&mut out, "- ", spans),
                DiffLine::Added(spans) => write_change(&mut out, "+ ", spans),
            }
        }
    }
    out
}

fn bench_unified_text(c: &mut Criterion) {
    let (small_before, small_after) = generate_sample_text(50, 10);
    let (med_before, med_after) = generate_sample_text(500, 10);
    let (large_before, large_after) = generate_sample_text(2000, 5);

    let med_hunks = compute_hunks(&med_before, &med_after);

    let mut group = c.benchmark_group("unified_text");

    group.bench_function("small_diff_50_lines", |b| {
        b.iter(|| {
            unified_text(
                black_box(&small_before),
                black_box(&small_after),
                black_box("Summary text"),
                black_box("path/to/file.rs"),
            )
        });
    });

    group.bench_function("medium_diff_500_lines", |b| {
        b.iter(|| {
            unified_text(
                black_box(&med_before),
                black_box(&med_after),
                black_box("Summary text"),
                black_box("path/to/file.rs"),
            )
        });
    });

    group.bench_function("large_diff_2000_lines", |b| {
        b.iter(|| {
            unified_text(
                black_box(&large_before),
                black_box(&large_after),
                black_box("Summary text"),
                black_box("path/to/file.rs"),
            )
        });
    });

    group.bench_function("render_500_lines_hunks_only", |b| {
        b.iter(|| {
            render_hunks_string(
                black_box("Summary text"),
                black_box("path/to/file.rs"),
                black_box(&med_hunks),
            )
        });
    });

    group.finish();
}

criterion_group!(benches, bench_unified_text);
criterion_main!(benches);
