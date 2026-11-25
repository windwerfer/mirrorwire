#![forbid(unsafe_code)]
//! `cargo xtask ci [--no-loopback]`: the local gate from AGENTS.md section 3 (fmt, clippy, tests,
//! crate-layer check, loopback). `cargo xtask layers` runs only the layer check.
//! `cargo audit` and `cargo deny` are separate installs and stay manual (see testing.md).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Allowed `mirrorwire-*` path dependencies per crate (architecture.md section 1).
/// Lower layers never depend on higher ones.
const LAYERS: &[(&str, &[&str])] = &[
    // Miracast spike crates (docs/agents/miracast.md section 7): standalone, platform-agnostic.
    ("miracast-nm", &[]),
    ("miracast-rtsp", &[]),
    ("miracast-rtp", &[]),
    ("miracast-loopback", &[]),
    ("mirrorwire-proto", &[]),
    ("mirrorwire-film", &[]),
    ("mirrorwire-media", &["mirrorwire-proto", "mirrorwire-film"]),
    ("mirrorwire-net", &["mirrorwire-proto"]),
    (
        "mirrorwire-app",
        &[
            "mirrorwire-proto",
            "mirrorwire-media",
            "mirrorwire-net",
            "mirrorwire-film",
        ],
    ),
    (
        "mirrorwire-android",
        &["mirrorwire-proto", "mirrorwire-net", "mirrorwire-film"],
    ),
];

/// Non-workspace crates proto may use (architecture.md: no I/O, async or FFmpeg).
const PROTO_EXTERNAL: &[&str] = &[
    "bytes",
    "serde",
    "serde_json",
    "anyhow",
    "reed-solomon-simd",
    "tracing",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(!args.iter().any(|a| a == "--no-loopback")),
        Some("layers") => layers(&root()),
        Some("loopback") => loopback(args.get(1).map_or("10", String::as_str)),
        _ => Err("usage: cargo xtask <ci [--no-loopback] | layers | loopback [secs]>".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    // xtask/ sits directly below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn run(name: &str, program: &str, args: &[&str]) -> Result<(), String> {
    eprintln!("\n==> {name}: {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(root())
        .status()
        .map_err(|e| format!("{name}: cannot start {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} failed ({status})"))
    }
}

fn ci(with_loopback: bool) -> Result<(), String> {
    run("fmt", "cargo", &["fmt", "--all", "--", "--check"])?;
    layers(&root())?;
    run(
        "clippy",
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run("test", "cargo", &["test", "--workspace"])?;
    // FFmpeg-free build (decisions D17): FilmCraft only.
    run(
        "clippy (no ffmpeg)",
        "cargo",
        &[
            "clippy",
            "-p",
            "mirrorwire-media",
            "-p",
            "mirrorwire-app",
            "--all-targets",
            "--no-default-features",
            "--features",
            "mirrorwire-app/gui,mirrorwire-app/filmcraft",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run(
        "test (no ffmpeg)",
        "cargo",
        &[
            "test",
            "-p",
            "mirrorwire-media",
            "--no-default-features",
            "--features",
            "filmcraft",
        ],
    )?;
    if with_loopback {
        loopback("10")?;
    }
    eprintln!("\nxtask ci: all gates passed (cargo audit / cargo deny run separately)");
    Ok(())
}

/// Runs the x264 loopback and checks its stats against `perf-budgets.toml`.
fn loopback(secs: &str) -> Result<(), String> {
    let budgets = fs::read_to_string(root().join("perf-budgets.toml"))
        .map_err(|e| format!("perf-budgets.toml: {e}"))?;
    let limits = parse_budgets(&budgets, "loopback");
    eprintln!("\n==> loopback: scripts/loopback-test.sh {secs} --encoder x264");
    let out = Command::new("scripts/loopback-test.sh")
        .args([secs, "--encoder", "x264"])
        .current_dir(root())
        .output()
        .map_err(|e| format!("loopback: cannot start script: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    if !out.status.success() {
        return Err(format!("loopback failed ({})", out.status));
    }
    check_loopback(&text, &limits)
}

/// `key = number` lines of one `[section]`, without a TOML parser (this crate has no deps).
fn parse_budgets(toml: &str, section: &str) -> Vec<(String, f64)> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in toml.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_section = line.trim_matches(|c| c == '[' || c == ']') == section;
        } else if let (true, Some((k, v))) = (in_section, line.split_once('=')) {
            if let Ok(v) = v.trim().parse::<f64>() {
                out.push((k.trim().to_string(), v));
            }
        }
    }
    out
}

/// Number following `label` in the text (first match).
fn number_after(text: &str, label: &str) -> Option<f64> {
    let rest = text.split_once(label)?.1;
    rest.split_whitespace().next()?.parse().ok()
}

/// The measured value for a budget key, from the viewer's loopback report.
fn measured(text: &str, key: &str) -> Option<f64> {
    match key {
        "capture_to_decoded_p95_ms_max" => {
            let line = text.lines().find(|l| l.starts_with("capture_to_decoded"))?;
            // columns: name mean p50 p95 max
            line.split_whitespace().nth(3)?.parse().ok()
        }
        "lost_max" => number_after(text, "lost "),
        "mismatches_max" => number_after(text, "mismatches "),
        "decode_errors_max" => number_after(text, "decode errors "),
        "unrecoverable_frames_max" => number_after(text, "unrecoverable frames "),
        _ => None,
    }
}

fn check_loopback(text: &str, limits: &[(String, f64)]) -> Result<(), String> {
    if limits.is_empty() {
        return Err("perf-budgets.toml has no [loopback] limits".to_string());
    }
    let mut errors = Vec::new();
    for (key, limit) in limits {
        match measured(text, key) {
            None => errors.push(format!("budget {key}: not found in the loopback report")),
            Some(v) if v > *limit => errors.push(format!("budget {key}: {v} > {limit}")),
            Some(v) => eprintln!("budget {key}: {v} <= {limit} ok"),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Dependency names declared in a manifest (`name = ...` / `name.workspace = true` lines inside
/// any `dependencies` table), without a TOML parser: this crate has no dependencies.
fn dependencies(manifest: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut out = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line.trim_end_matches(']').ends_with("dependencies");
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let name = key.trim().split('.').next().unwrap_or("").trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn layers(root: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    for (krate, allowed) in LAYERS {
        let path = root.join("crates").join(krate).join("Cargo.toml");
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let deps = dependencies(&text);
        for dep in deps.iter().filter(|d| d.starts_with("mirrorwire-")) {
            if !allowed.contains(&dep.as_str()) {
                errors.push(format!(
                    "{krate} must not depend on {dep} (architecture.md layers)"
                ));
            }
        }
        if *krate == "mirrorwire-proto" {
            for dep in deps
                .iter()
                .filter(|d| !PROTO_EXTERNAL.contains(&d.as_str()))
            {
                errors.push(format!(
                    "{krate} must not depend on {dep} (allowed: {PROTO_EXTERNAL:?})"
                ));
            }
        }
    }
    // A new crate must be added to LAYERS.
    if let Ok(entries) = fs::read_dir(root.join("crates")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !LAYERS.iter().any(|(k, _)| *k == name) {
                errors.push(format!("crates/{name} is not listed in xtask LAYERS"));
            }
        }
    }
    if errors.is_empty() {
        eprintln!("layers: ok");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dependency_names() {
        let m = "[package]\nname = \"x\"\n[dependencies]\nbytes.workspace = true\n# c\nserde = { version = \"1\" }\n[dev-dependencies]\nfoo = \"1\"\n[features]\nbar = []\n";
        assert_eq!(dependencies(m), vec!["bytes", "serde", "foo"]);
    }

    const REPORT: &str = "lost 0  skipped-until-keyframe 0  keyframe requests 0  decode errors 0\nframe marker checks 359  mismatches 0\nfec overhead 12%  packet loss 0.00% (0 of 7461)  rebuilt packets 0  unrecoverable frames 0\ncapture_to_decoded      12.72    12.05    15.12    77.73\n";

    #[test]
    fn budgets_pass_and_fail() {
        let t = "[other]\nlost_max = 9\n[loopback]\ncapture_to_decoded_p95_ms_max = 20 # c\nlost_max = 0\n";
        let l = parse_budgets(t, "loopback");
        assert_eq!(l.len(), 2);
        assert!(check_loopback(REPORT, &l).is_ok());
        let tight = parse_budgets(
            "[loopback]\ncapture_to_decoded_p95_ms_max = 10\n",
            "loopback",
        );
        assert!(check_loopback(REPORT, &tight)
            .unwrap_err()
            .contains("15.12 > 10"));
        let bad = REPORT.replace("lost 0", "lost 3");
        assert!(check_loopback(&bad, &l).is_err());
        let unknown = parse_budgets("[loopback]\nbogus_max = 1\n", "loopback");
        assert!(check_loopback(REPORT, &unknown)
            .unwrap_err()
            .contains("not found"));
    }

    #[test]
    fn the_real_workspace_obeys_the_crate_layers() {
        assert_eq!(layers(&root()), Ok(()));
        assert!(root().join("Cargo.toml").is_file());
    }

    /// A throwaway workspace tree under the system temp dir with the given manifests.
    fn fake_tree(name: &str, manifests: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (krate, text) in manifests {
            let d = dir.join("crates").join(krate);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("Cargo.toml"), text).unwrap();
        }
        dir
    }

    #[test]
    fn layers_rejects_a_forbidden_dependency_and_an_unlisted_crate() {
        let manifests: Vec<(&str, String)> = LAYERS
            .iter()
            .map(|(k, _)| (*k, "[package]\nname = \"x\"\n".to_string()))
            .collect();
        let mut refs: Vec<(&str, &str)> = manifests.iter().map(|(k, t)| (*k, t.as_str())).collect();
        // The proto crate may depend on nothing from the workspace.
        let bad = "[dependencies]\nmirrorwire-net.workspace = true\n";
        refs.retain(|(k, _)| *k != "mirrorwire-proto");
        refs.push(("mirrorwire-proto", bad));
        refs.push(("surprise", "[package]\n"));
        let dir = fake_tree("layers", &refs);
        let err = layers(&dir).unwrap_err();
        assert!(
            err.contains("mirrorwire-proto must not depend on mirrorwire-net"),
            "{err}"
        );
        assert!(err.contains("crates/surprise is not listed"), "{err}");
        let _ = fs::remove_dir_all(&dir);
        // A missing manifest is reported with its path, not a panic.
        let empty = fake_tree("empty", &[("mirrorwire-proto", "")]);
        assert!(layers(&empty).unwrap_err().contains("Cargo.toml"));
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn run_reports_success_failure_and_a_missing_program() {
        assert_eq!(run("ok", "true", &[]), Ok(()));
        assert!(run("bad", "false", &[])
            .unwrap_err()
            .starts_with("bad failed"));
        let err = run("none", "definitely-not-a-program-xtask", &[]).unwrap_err();
        assert!(err.contains("cannot start"), "{err}");
    }

    #[test]
    fn report_numbers_are_read_by_label() {
        assert_eq!(number_after(REPORT, "decode errors "), Some(0.0));
        assert_eq!(number_after(REPORT, "frame marker checks "), Some(359.0));
        assert_eq!(number_after(REPORT, "no such label "), None);
        assert_eq!(
            measured(REPORT, "capture_to_decoded_p95_ms_max"),
            Some(15.12)
        );
        assert_eq!(measured(REPORT, "unrecoverable_frames_max"), Some(0.0));
        assert_eq!(measured(REPORT, "bogus"), None);
        assert!(check_loopback(REPORT, &[]).is_err());
    }
}
