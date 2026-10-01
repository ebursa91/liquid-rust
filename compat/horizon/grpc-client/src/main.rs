use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use horizon_grpc_client::{fetch, validate_query, ClientResult, GetRenderContextRequest};
use serde_json::{json, Value as Json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

fn options() -> ClientResult<BTreeMap<String, String>> {
    let mut options = BTreeMap::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(key) = arguments.next() {
        if ![
            "--endpoint",
            "--tenant-id",
            "--storefront-id",
            "--locale",
            "--configuration",
            "--page",
            "--current-page",
            "--cart-id",
            "--request-id",
            "--token-env",
            "--timeout-ms",
            "--render-timeout-ms",
            "--renderer",
            "--theme-root",
            "--output-dir",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("Unknown native RPC option {key}").into());
        }
        let value = arguments
            .next()
            .ok_or("Every RPC option requires a value")?;
        if options.insert(key, value).is_some() {
            return Err("Duplicate RPC option".into());
        }
    }
    Ok(options)
}

fn required<'a>(options: &'a BTreeMap<String, String>, key: &str) -> ClientResult<&'a str> {
    options
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("{key} is required").into())
}

fn selected<'a>(options: &'a BTreeMap<String, String>, key: &str, default: &'a str) -> &'a str {
    options.get(key).map(String::as_str).unwrap_or(default)
}

fn checked_path(path: &Path) -> ClientResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut checked = PathBuf::new();
    for component in absolute.components() {
        if component == Component::ParentDir {
            return Err("RPC paths must not contain parent-directory traversal".into());
        }
        if component == Component::CurDir {
            continue;
        }
        checked.push(component);
        match std::fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("RPC paths and their ancestors must not be symlinks".into())
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(checked)
}

fn validate_output(
    directory: &Path,
    theme: &Path,
    renderer: &Path,
) -> ClientResult<(PathBuf, PathBuf, PathBuf)> {
    let directory = checked_path(directory)?;
    let theme = checked_path(theme)?;
    let renderer = checked_path(renderer)?;
    if !theme.is_dir() || !renderer.is_file() {
        return Err("RPC theme directory and renderer executable must exist".into());
    }
    if directory.starts_with(&theme)
        || theme.starts_with(&directory)
        || renderer.starts_with(&directory)
    {
        return Err("RPC output directory overlaps an input".into());
    }
    if directory.exists() && !directory.is_dir() {
        return Err("RPC output directory must be a real directory".into());
    }
    for name in ["rpc-report.json", "report.json", "index.html", "styles.css"] {
        let artifact = directory.join(name);
        if artifact.is_symlink() || (artifact.exists() && !artifact.is_file()) {
            return Err("RPC output artifacts must be regular files".into());
        }
    }
    Ok((directory, theme, renderer))
}

fn clear_success_markers(directory: &Path) -> ClientResult<()> {
    for name in ["rpc-report.json", "report.json"] {
        let marker = directory.join(name);
        if marker.exists() {
            std::fs::remove_file(marker)?;
        }
    }
    Ok(())
}

fn verify_render_report(directory: &Path, expected_sha: &Json) -> ClientResult<()> {
    let result = (|| -> ClientResult<()> {
        let report: Json = serde_json::from_slice(&std::fs::read(directory.join("report.json"))?)?;
        if !report["error"].is_null() || report["fixture_sha256"] != *expected_sha {
            return Err(
                "Rendered fixture identity differs from verified authoritative RPC context".into(),
            );
        }
        Ok(())
    })();
    if result.is_err() {
        clear_success_markers(directory)?;
    }
    result
}

async fn render_snapshot(
    renderer: &Path,
    theme: &Path,
    directory: &Path,
    page: &str,
    context: &[u8],
    timeout: Duration,
) -> ClientResult<()> {
    let mut child = Command::new(renderer)
        .args([
            "--fixture-stdin",
            "--scope",
            "page",
            "--page",
            page,
            "--theme-root",
        ])
        .arg(theme)
        .arg("--output-dir")
        .arg(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().ok_or("Missing renderer stdin")?;
    let mut output = child.stdout.take().ok_or("Missing renderer stdout")?;
    let mut errors = child.stderr.take().ok_or("Missing renderer stderr")?;
    let result = tokio::time::timeout(timeout, async {
        let write = async {
            input.write_all(context).await?;
            drop(input);
            Ok::<_, std::io::Error>(())
        };
        let read_output = async {
            let mut bytes = Vec::new();
            output.read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let read_errors = async {
            let mut bytes = Vec::new();
            errors.read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let (write, output, errors, status) =
            tokio::join!(write, read_output, read_errors, child.wait());
        write?;
        output?;
        let errors = errors?;
        let status = status?;
        if !status.success() {
            return Err(format!(
                "Rust renderer failed after verified RPC fetch: {}",
                String::from_utf8_lossy(&errors)
                    .chars()
                    .take(2048)
                    .collect::<String>()
            )
            .into());
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            // Explicitly kill and reap; cancellation also retains kill-on-drop.
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("Rust renderer deadline exceeded; no local fallback".into())
        }
    }
}

#[tokio::main]
async fn main() -> ClientResult<()> {
    let options = options()?;
    let (directory, theme, renderer) = validate_output(
        Path::new(required(&options, "--output-dir")?),
        Path::new(required(&options, "--theme-root")?),
        Path::new(required(&options, "--renderer")?),
    )?;
    // Once the output is safe, invalid request/token options must not retain success.
    clear_success_markers(&directory)?;
    let page = selected(&options, "--page", "index");
    let query = GetRenderContextRequest {
        tenant_id: required(&options, "--tenant-id")?.to_owned(),
        storefront_id: required(&options, "--storefront-id")?.to_owned(),
        locale: selected(&options, "--locale", "en").to_owned(),
        configuration: selected(&options, "--configuration", "published").to_owned(),
        page: page.to_owned(),
        current_page: selected(&options, "--current-page", "1").parse()?,
        cart_id: selected(&options, "--cart-id", "default").to_owned(),
        request_id: required(&options, "--request-id")?.to_owned(),
    };
    let timeout = Duration::from_millis(selected(&options, "--timeout-ms", "5000").parse()?);
    validate_query(&query, timeout)?;
    let render_timeout =
        Duration::from_millis(selected(&options, "--render-timeout-ms", "60000").parse()?);
    if render_timeout.is_zero() || render_timeout > Duration::from_secs(300) {
        return Err("Renderer timeout must be positive and at most 300 seconds".into());
    }
    let token_name = selected(&options, "--token-env", "HORIZON_STORE_TOKEN");
    let token =
        std::env::var(token_name).map_err(|_| "RPC token environment variable is missing")?;
    let marker = directory.join("rpc-report.json");
    let snapshot = fetch(required(&options, "--endpoint")?, &token, query, timeout).await?;
    let started = Instant::now();
    if let Err(error) = render_snapshot(
        &renderer,
        &theme,
        &directory,
        page,
        &snapshot.context_json,
        render_timeout,
    )
    .await
    {
        clear_success_markers(&directory)?;
        return Err(error);
    }
    verify_render_report(&directory, &snapshot.metadata["context_sha256"])?;
    let mut metadata = snapshot.metadata;
    metadata["renderer_process_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    metadata["render_timeout_ms"] = json!(render_timeout.as_millis());
    metadata["renderer"] = json!("liquid-rust original Horizon source");
    metadata["local_fixture_fallback"] = json!(false);
    use std::io::Write;
    let temporary = directory.join(format!(
        ".rpc-report-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(format!("{}\n", serde_json::to_string_pretty(&metadata)?).as_bytes())?;
    drop(file);
    std::fs::rename(temporary, marker)?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"action":"rpc_render_complete","metadata":metadata,"output_dir":directory})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "horizon-rpc-driver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn output_input_overlap_is_rejected_before_stale_markers_are_removed() {
        let directory = directory();
        let theme = directory.join("theme");
        let renderer = directory.join("renderer");
        std::fs::create_dir(&theme).unwrap();
        std::fs::write(&renderer, "original mock executable").unwrap();
        std::fs::write(theme.join("report.json"), "stale input marker").unwrap();
        assert!(validate_output(&theme, &theme, &renderer).is_err());
        assert_eq!(
            std::fs::read_to_string(theme.join("report.json")).unwrap(),
            "stale input marker"
        );
        let output = directory.join("output");
        std::fs::create_dir(&output).unwrap();
        for name in ["report.json", "rpc-report.json"] {
            std::fs::write(output.join(name), "stale success").unwrap();
        }
        validate_output(&output, &theme, &renderer).unwrap();
        clear_success_markers(&output).unwrap();
        assert!(!output.join("report.json").exists());
        assert!(!output.join("rpc-report.json").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn malformed_or_mismatched_renderer_reports_remove_success_markers() {
        let directory = directory();
        for report in [
            "{broken",
            r#"{"error":null,"fixture_sha256":"different"}"#,
            r#"{"error":{},"fixture_sha256":"expected"}"#,
        ] {
            std::fs::write(directory.join("report.json"), report).unwrap();
            std::fs::write(directory.join("rpc-report.json"), "stale RPC success").unwrap();
            assert!(verify_render_report(&directory, &json!("expected")).is_err());
            assert!(!directory.join("report.json").exists());
            assert!(!directory.join("rpc-report.json").exists());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn output_ancestors_and_broken_artifact_symlinks_are_rejected() {
        let directory = directory();
        let theme = directory.join("theme");
        let renderer = directory.join("renderer");
        std::fs::create_dir(&theme).unwrap();
        std::fs::write(&renderer, "original mock executable").unwrap();
        let output = directory.join("output");
        std::fs::create_dir(&output).unwrap();
        std::os::unix::fs::symlink(&output, directory.join("alias")).unwrap();
        assert!(validate_output(&directory.join("alias/child"), &theme, &renderer).is_err());
        std::os::unix::fs::symlink(directory.join("missing"), output.join("rpc-report.json"))
            .unwrap();
        assert!(validate_output(&output, &theme, &renderer).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn renderer_deadline_kills_and_reaps_the_child_and_failures_are_visible() {
        use std::os::unix::fs::PermissionsExt;
        let directory = directory();
        let renderer = directory.join("renderer");
        let pid_file = directory.join("child.pid");
        std::fs::write(
            &renderer,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&renderer, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let error = render_snapshot(
            &renderer,
            &directory,
            &directory,
            "index",
            b"{}",
            Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("deadline exceeded"));
        assert!(started.elapsed() < Duration::from_secs(2));
        #[cfg(target_os = "linux")]
        assert!(!PathBuf::from(format!(
            "/proc/{}",
            std::fs::read_to_string(&pid_file).unwrap()
        ))
        .exists());
        std::fs::write(
            &renderer,
            "#!/bin/sh\ncat >/dev/null\nprintf 'original render failure' >&2\nexit 42\n",
        )
        .unwrap();
        let error = render_snapshot(
            &renderer,
            &directory,
            &directory,
            "index",
            b"{}",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("original render failure"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
