//! Failure report rendering (markdown / JSON) and export helpers.
//!
//! Reports omit error messages as well as prompts, bodies, and credentials because upstream
//! diagnostics can echo request data.

use crate::store::FailureRecord;
use chrono::{DateTime, Local, Utc};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Metadata header for a failure report export.
#[derive(Clone, Debug)]
pub struct ReportMeta {
    pub version: String,
    pub listen: String,
    pub generated: DateTime<Utc>,
    /// Filter label as shown in the monitor (e.g. "All", "ProxyAssemble").
    pub filter: String,
}

impl ReportMeta {
    pub fn new(
        version: impl Into<String>,
        listen: impl Into<String>,
        filter: impl Into<String>,
    ) -> Self {
        Self {
            version: version.into(),
            listen: listen.into(),
            generated: Utc::now(),
            filter: filter.into(),
        }
    }
}

/// Count failures by [`FailureKind`] (stable kind-name order via BTreeMap).
pub fn summary_counts(records: &[FailureRecord]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for r in records {
        *counts.entry(r.kind.as_str()).or_insert(0) += 1;
    }
    counts
}

/// Render a markdown failure report without diagnostic messages, prompts, bodies, or credentials.
pub fn render_markdown(records: &[FailureRecord], meta: &ReportMeta) -> String {
    let counts = summary_counts(records);
    let mut out = String::new();
    out.push_str("# grok-build-proxy failure report\n");
    out.push_str(&format!("- generated: {}\n", meta.generated.to_rfc3339()));
    out.push_str(&format!("- version: {}\n", meta.version));
    out.push_str(&format!("- listen: {}\n", meta.listen));
    out.push_str(&format!(
        "- window: last {} failures (filter: {})\n",
        records.len(),
        meta.filter
    ));
    out.push('\n');
    out.push_str("## Summary\n");
    out.push_str("| kind | count |\n");
    out.push_str("| --- | --- |\n");
    if counts.is_empty() {
        out.push_str("| _(none)_ | 0 |\n");
    } else {
        for (kind, n) in &counts {
            out.push_str(&format!("| {kind} | {n} |\n"));
        }
    }
    out.push('\n');
    out.push_str("## Failures\n");
    if records.is_empty() {
        out.push_str("\n_(no failures in current filter)_\n");
    } else {
        for (i, r) in records.iter().enumerate() {
            out.push_str(&format_failure_md(i + 1, r));
        }
    }
    out.push('\n');
    out.push_str("(no error message, prompt, or response body included)\n");
    out
}

fn format_failure_md(index: usize, r: &FailureRecord) -> String {
    let etype = if r.error_type.is_empty() {
        r.kind.as_str()
    } else {
        r.error_type.as_str()
    };
    let resp = if r.response_id.is_empty() {
        "-"
    } else {
        r.response_id.as_str()
    };
    format!(
        "\n### {index}. {etype}\n\
         - ts: {}\n\
         - request_id: {}\n\
         - session_id: {}\n\
         - model: {} → {}\n\
         - kind: {}\n\
         - status: {}\n\
         - duration_ms: {}\n\
         - attempt: {}\n\
         - auth_retried: {}\n\
         - session_failure_index: {}\n\
         - response_id: {}\n\
         - mapped: {}  lite: {}  fast: {}\n\
         - outputs: {}\n\
         - capture_bytes: {}\n\
         - request_body_bytes: {}\n\
         - input_item_count: {}\n\
         - proxy_prepare_ms: {}\n\
         - credential_ms: {}\n\
         - upstream_headers_ms: {}\n\
         - first_chunk_ms: {}\n\
         - request_fingerprint: {}\n\
         - retry_candidate: {}\n\
         - error_type: {}\n",
        r.ts.to_rfc3339(),
        r.request_id,
        r.session_id,
        r.requested_model,
        r.model,
        r.kind.as_str(),
        r.status_code,
        r.duration_ms,
        r.attempt,
        r.auth_retried,
        r.session_failure_index,
        resp,
        r.mapped,
        r.lite,
        r.fast,
        r.output_count,
        r.capture_bytes,
        r.diagnostics.request_body_bytes,
        r.diagnostics.input_item_count,
        r.diagnostics.proxy_prepare_ms,
        r.diagnostics.credential_ms,
        r.diagnostics.upstream_headers_ms,
        r.diagnostics.first_chunk_ms,
        r.diagnostics.request_fingerprint,
        r.retry_candidate,
        etype,
    )
}

/// Render a JSON failure report without diagnostic messages, prompts, bodies, or credentials.
pub fn render_json(records: &[FailureRecord], meta: &ReportMeta) -> String {
    let counts = summary_counts(records);
    let mut summary = Map::new();
    for (kind, n) in counts {
        summary.insert(kind.to_owned(), json!(n));
    }
    let failures: Vec<Value> = records.iter().map(failure_to_json).collect();
    let doc = json!({
        "meta": {
            "generated": meta.generated.to_rfc3339(),
            "version": meta.version,
            "listen": meta.listen,
            "filter": meta.filter,
            "failure_count": records.len(),
            "summary": summary,
        },
        "failures": failures,
    });
    // pretty for human paste into issues
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
}

fn failure_to_json(r: &FailureRecord) -> Value {
    json!({
        "ts": r.ts.to_rfc3339(),
        "request_id": r.request_id,
        "session_id": r.session_id,
        "requested_model": r.requested_model,
        "model": r.model,
        "status_code": r.status_code,
        "duration_ms": r.duration_ms,
        "kind": r.kind.as_str(),
        "error_type": r.error_type,
        "response_id": r.response_id,
        "mapped": r.mapped,
        "lite": r.lite,
        "fast": r.fast,
        "auth_retried": r.auth_retried,
        "attempt": r.attempt,
        "output_count": r.output_count,
        "capture_bytes": r.capture_bytes,
        "request_body_bytes": r.diagnostics.request_body_bytes,
        "input_item_count": r.diagnostics.input_item_count,
        "proxy_prepare_ms": r.diagnostics.proxy_prepare_ms,
        "credential_ms": r.diagnostics.credential_ms,
        "upstream_headers_ms": r.diagnostics.upstream_headers_ms,
        "first_chunk_ms": r.diagnostics.first_chunk_ms,
        "request_fingerprint": r.diagnostics.request_fingerprint,
        "retry_candidate": r.retry_candidate,
        "session_failure_index": r.session_failure_index,
    })
}

/// Default report directory: `$HOME/.grok/proxy-reports`.
pub fn default_report_dir() -> io::Result<PathBuf> {
    let home = env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HOME must be an absolute path",
        ));
    }
    Ok(home.join(".grok").join("proxy-reports"))
}

/// Timestamped report filename stem: `failure-YYYYMMDD-HHMMSS`.
pub fn report_filename_stem(when: DateTime<Utc>) -> String {
    let local = when.with_timezone(&Local);
    format!("failure-{}", local.format("%Y%m%d-%H%M%S"))
}

/// Write report body to `dir/stem.ext` (or `stem-N.ext` if taken), creating parent dirs.
///
/// On Unix, the directory is `0o700` and files are `0o600` (create-new, no silent overwrite).
pub fn write_report_file(dir: &Path, stem: &str, ext: &str, body: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }

    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match write_new_private(&path, body.as_bytes()) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(
        "could not allocate unique report filename after 1000 attempts",
    ))
}

fn write_new_private(path: &Path, body: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(body)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "report file exists",
            ));
        }
        fs::write(path, body)
    }
}

/// Copy text to the macOS clipboard via `pbcopy`.
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "pbcopy stdin missing"))?;
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "pbcopy exited with status {status}"
        )))
    }
}

/// Result of a clipboard/file export action (for footer toast).
#[derive(Clone, Debug)]
pub enum ExportOutcome {
    Copied {
        count: usize,
        json: bool,
    },
    Written {
        path: PathBuf,
        count: usize,
        json: bool,
    },
    Empty,
    Error(String),
}

impl ExportOutcome {
    pub fn toast(&self) -> String {
        match self {
            Self::Copied { count, json } => {
                let fmt = if *json { "json" } else { "md" };
                format!("copied {count} failures ({fmt}) to clipboard")
            }
            Self::Written { path, count, json } => {
                let fmt = if *json { "json" } else { "md" };
                format!("wrote {count} failures ({fmt}) → {}", path.display())
            }
            Self::Empty => "no failures to export (current filter)".into(),
            Self::Error(msg) => format!("export failed: {msg}"),
        }
    }
}

/// Export markdown or JSON to the clipboard without creating a file on failure.
pub fn export_copy(records: &[FailureRecord], meta: &ReportMeta, json: bool) -> ExportOutcome {
    export_copy_with(records, meta, json, copy_to_clipboard)
}

/// Testable export-copy path: `copy` is injected (production uses [`copy_to_clipboard`]).
pub(crate) fn export_copy_with(
    records: &[FailureRecord],
    meta: &ReportMeta,
    json: bool,
    copy: impl FnOnce(&str) -> io::Result<()>,
) -> ExportOutcome {
    if records.is_empty() {
        return ExportOutcome::Empty;
    }
    let body = if json {
        render_json(records, meta)
    } else {
        render_markdown(records, meta)
    };
    match copy(&body) {
        Ok(()) => ExportOutcome::Copied {
            count: records.len(),
            json,
        },
        Err(e) => ExportOutcome::Error(format!("clipboard: {e}")),
    }
}

/// Export markdown or JSON by writing under the default report directory.
pub fn export_write(records: &[FailureRecord], meta: &ReportMeta, json: bool) -> ExportOutcome {
    if records.is_empty() {
        return ExportOutcome::Empty;
    }
    match default_report_dir() {
        Ok(dir) => export_write_to(records, meta, json, &dir),
        Err(e) => ExportOutcome::Error(e.to_string()),
    }
}

/// Testable write path that avoids touching the user's report directory.
pub(crate) fn export_write_to(
    records: &[FailureRecord],
    meta: &ReportMeta,
    json: bool,
    dir: &Path,
) -> ExportOutcome {
    if records.is_empty() {
        return ExportOutcome::Empty;
    }
    let body = if json {
        render_json(records, meta)
    } else {
        render_markdown(records, meta)
    };
    let stem = report_filename_stem(meta.generated);
    let ext = if json { "json" } else { "md" };
    match write_report_file(dir, &stem, ext, &body) {
        Ok(path) => ExportOutcome::Written {
            path,
            count: records.len(),
            json,
        },
        Err(e) => ExportOutcome::Error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FailureKind;
    use chrono::TimeZone;

    fn sample_record(kind: FailureKind, session: &str, req: &str) -> FailureRecord {
        FailureRecord {
            ts: Utc.with_ymd_and_hms(2026, 7, 18, 12, 34, 56).unwrap(),
            request_id: req.into(),
            session_id: session.into(),
            requested_model: "alias".into(),
            model: "gpt-test".into(),
            status_code: 502,
            duration_ms: 1823,
            kind,
            error_type: "upstream_http".into(),
            error_message: "bad gateway".into(),
            response_id: "resp_x".into(),
            mapped: true,
            lite: true,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 2,
            capture_bytes: 4096,
            session_failure_index: 1,
            diagnostics: crate::events::RequestDiagnostics {
                request_body_bytes: 1234,
                input_item_count: 9,
                proxy_prepare_ms: 3,
                credential_ms: 4,
                upstream_headers_ms: 5,
                first_chunk_ms: 8,
                request_fingerprint: "fp-safe".into(),
            },
            retry_candidate: true,
        }
    }

    #[test]
    fn markdown_includes_meta_summary_and_fields() {
        let mut assemble = sample_record(FailureKind::ProxyAssemble, "sess-a", "req-1");
        assemble.error_type = "proxy_incomplete_output".into();
        assemble.error_message = "could not assemble".into();
        assemble.status_code = 200;
        let mut third = sample_record(FailureKind::ProxyAssemble, "sess-b", "req-3");
        third.error_type = "proxy_incomplete_output".into();
        let records = [
            assemble,
            sample_record(FailureKind::UpstreamHttp, "sess-a", "req-2"),
            third,
        ];

        let meta = ReportMeta {
            version: "0.0.12".into(),
            listen: "127.0.0.1:18765".into(),
            generated: Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap(),
            filter: "All".into(),
        };
        let md = render_markdown(&records, &meta);
        assert!(md.contains("# grok-build-proxy failure report"));
        assert!(md.contains("version: 0.0.12"));
        assert!(md.contains("listen: 127.0.0.1:18765"));
        assert!(md.contains("filter: All"));
        assert!(md.contains("| ProxyAssemble | 2 |"));
        assert!(md.contains("| UpstreamHttp | 1 |"));
        assert!(md.contains("request_id: req-1"));
        assert!(md.contains("session_id: sess-a"));
        assert!(md.contains("duration_ms: 1823"));
        assert!(md.contains("auth_retried: false"));
        assert!(md.contains("capture_bytes: 4096"));
        assert!(md.contains("request_body_bytes: 1234"));
        assert!(md.contains("input_item_count: 9"));
        assert!(md.contains("proxy_prepare_ms: 3"));
        assert!(md.contains("credential_ms: 4"));
        assert!(md.contains("upstream_headers_ms: 5"));
        assert!(md.contains("first_chunk_ms: 8"));
        assert!(md.contains("request_fingerprint: fp-safe"));
        assert!(md.contains("retry_candidate: true"));
        assert!(md.contains("(no error message, prompt, or response body included)"));
        // The stored diagnostic may contain upstream-echoed secrets; reports must omit it.
        assert!(!md.contains("could not assemble"));
        assert!(!md.contains("bad gateway"));
        assert!(!md.contains("message:"));
        assert!(!md.contains("prompt:"));
        assert!(!md.contains("\"prompt\""));
    }

    #[test]
    fn json_round_shape_and_no_secrets() {
        let mut record = sample_record(FailureKind::AuthRetryFailed, "s", "r");
        record.error_message = "upstream echoed access_token=secret".into();
        let records = vec![record];
        let meta = ReportMeta::new("0.0.12", "127.0.0.1:1", "Auth");
        let s = render_json(&records, &meta);
        let v: Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(v["meta"]["filter"], "Auth");
        assert_eq!(v["meta"]["failure_count"], 1);
        assert_eq!(v["meta"]["summary"]["AuthRetryFailed"], 1);
        assert_eq!(v["failures"][0]["kind"], "AuthRetryFailed");
        assert_eq!(v["failures"][0]["request_id"], "r");
        assert_eq!(v["failures"][0]["request_body_bytes"], 1234);
        assert_eq!(v["failures"][0]["input_item_count"], 9);
        assert_eq!(v["failures"][0]["proxy_prepare_ms"], 3);
        assert_eq!(v["failures"][0]["credential_ms"], 4);
        assert_eq!(v["failures"][0]["upstream_headers_ms"], 5);
        assert_eq!(v["failures"][0]["first_chunk_ms"], 8);
        assert_eq!(v["failures"][0]["request_fingerprint"], "fp-safe");
        assert_eq!(v["failures"][0]["retry_candidate"], true);
        assert!(v["failures"][0].get("error_message").is_none());
        let text = s.to_lowercase();
        assert!(!text.contains("access_token"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("\"prompt\""));
    }

    #[test]
    fn empty_report_still_valid() {
        let meta = ReportMeta::new("0.0.12", "127.0.0.1:1", "Stream");
        let md = render_markdown(&[], &meta);
        assert!(md.contains("last 0 failures"));
        assert!(md.contains("_(no failures"));
        let j = render_json(&[], &meta);
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["meta"]["failure_count"], 0);
        assert!(v["failures"].as_array().unwrap().is_empty());
    }

    #[test]
    fn write_report_file_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("proxy-reports");
        let path = write_report_file(nested.as_path(), "failure-test", "md", "# hi\n").unwrap();
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# hi\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "report file should be user-only");
            let dmode = fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
            assert_eq!(dmode, 0o700, "report dir should be user-only");
        }
    }

    #[test]
    fn write_report_file_disambiguates_same_stem() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_report_file(dir.path(), "failure-dup", "md", "one").unwrap();
        let b = write_report_file(dir.path(), "failure-dup", "md", "two").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap(), "failure-dup.md");
        assert_eq!(b.file_name().unwrap(), "failure-dup-1.md");
        assert_eq!(fs::read_to_string(&a).unwrap(), "one");
        assert_eq!(fs::read_to_string(&b).unwrap(), "two");
    }

    #[test]
    fn filename_stem_format() {
        let ts = Utc.with_ymd_and_hms(2026, 7, 18, 15, 4, 5).unwrap();
        let stem = report_filename_stem(ts);
        assert!(stem.starts_with("failure-"), "{stem}");
        // Local timezone may shift the clock; still YYYYMMDD-HHMMSS shape after prefix.
        let rest = &stem["failure-".len()..];
        assert_eq!(rest.len(), 15, "{stem}");
        assert_eq!(&rest[8..9], "-");
    }

    #[test]
    fn export_empty_is_empty_outcome() {
        let meta = ReportMeta::new("0.0.12", "l", "All");
        assert!(matches!(
            export_copy(&[], &meta, false),
            ExportOutcome::Empty
        ));
        assert!(matches!(
            export_write(&[], &meta, true),
            ExportOutcome::Empty
        ));
    }

    #[test]
    fn export_copy_error_does_not_write_a_file() {
        let records = [sample_record(FailureKind::UpstreamHttp, "s", "r1")];
        let meta = ReportMeta::new("0.0.12", "127.0.0.1:1", "All");
        let out = export_copy_with(&records, &meta, false, |_| {
            Err(io::Error::other("forced clipboard failure"))
        });

        assert!(matches!(out, ExportOutcome::Error(_)), "{out:?}");
        assert!(out.toast().contains("clipboard"));
    }
}
