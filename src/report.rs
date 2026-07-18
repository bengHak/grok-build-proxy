//! Failure report rendering (markdown / JSON) and export helpers.
//!
//! Reports never include prompt/body/credentials — only FailureRecord metadata.

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

/// Render a markdown failure report (no prompt/body/credentials).
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
    out.push_str("(no prompt/response body included)\n");
    out
}

fn format_failure_md(index: usize, r: &FailureRecord) -> String {
    let etype = if r.error_type.is_empty() {
        r.kind.as_str()
    } else {
        r.error_type.as_str()
    };
    let msg = if r.error_message.is_empty() {
        "-"
    } else {
        r.error_message.as_str()
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
         - error_type: {}\n\
         - message: {}\n",
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
        etype,
        msg,
    )
}

/// Render a JSON failure report (no prompt/body/credentials).
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
        "error_message": r.error_message,
        "response_id": r.response_id,
        "mapped": r.mapped,
        "lite": r.lite,
        "fast": r.fast,
        "auth_retried": r.auth_retried,
        "attempt": r.attempt,
        "output_count": r.output_count,
        "capture_bytes": r.capture_bytes,
        "session_failure_index": r.session_failure_index,
    })
}

/// Default report directory: `$HOME/.grok/proxy-reports`.
pub fn default_report_dir() -> io::Result<PathBuf> {
    let home = env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(PathBuf::from(home).join(".grok").join("proxy-reports"))
}

/// Timestamped report filename stem: `failure-YYYYMMDD-HHMMSS`.
pub fn report_filename_stem(when: DateTime<Utc>) -> String {
    let local = when.with_timezone(&Local);
    format!("failure-{}", local.format("%Y%m%d-%H%M%S"))
}

/// Write report body to `dir/stem.ext`, creating parent dirs as needed.
pub fn write_report_file(dir: &Path, stem: &str, ext: &str, body: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.{ext}"));
    fs::write(&path, body)?;
    Ok(path)
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

/// Export markdown or JSON: copy to clipboard, falling back to a written file.
pub fn export_copy(records: &[FailureRecord], meta: &ReportMeta, json: bool) -> ExportOutcome {
    if records.is_empty() {
        return ExportOutcome::Empty;
    }
    let body = if json {
        render_json(records, meta)
    } else {
        render_markdown(records, meta)
    };
    match copy_to_clipboard(&body) {
        Ok(()) => ExportOutcome::Copied {
            count: records.len(),
            json,
        },
        Err(e) => {
            // Fallback: write file and surface path in toast.
            match write_report(meta, json, &body) {
                Ok(path) => ExportOutcome::Written {
                    path,
                    count: records.len(),
                    json,
                },
                Err(werr) => ExportOutcome::Error(format!("clipboard: {e}; write: {werr}")),
            }
        }
    }
}

/// Export markdown or JSON by writing under the default report directory.
pub fn export_write(records: &[FailureRecord], meta: &ReportMeta, json: bool) -> ExportOutcome {
    if records.is_empty() {
        return ExportOutcome::Empty;
    }
    let body = if json {
        render_json(records, meta)
    } else {
        render_markdown(records, meta)
    };
    match write_report(meta, json, &body) {
        Ok(path) => ExportOutcome::Written {
            path,
            count: records.len(),
            json,
        },
        Err(e) => ExportOutcome::Error(e.to_string()),
    }
}

fn write_report(meta: &ReportMeta, json: bool, body: &str) -> io::Result<PathBuf> {
    let dir = default_report_dir()?;
    let stem = report_filename_stem(meta.generated);
    let ext = if json { "json" } else { "md" };
    write_report_file(&dir, &stem, ext, body)
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
        assert!(md.contains("(no prompt/response body included)"));
        // Must not invent body/credentials fields.
        assert!(!md.to_lowercase().contains("authorization"));
        assert!(!md.contains("refresh_token"));
        assert!(!md.contains("access_token"));
        // Disclaimer may mention "prompt"; field dumps must not.
        assert!(!md.contains("prompt:"));
        assert!(!md.contains("\"prompt\""));
    }

    #[test]
    fn json_round_shape_and_no_secrets() {
        let records = vec![sample_record(FailureKind::AuthRetryFailed, "s", "r")];
        let meta = ReportMeta::new("0.0.12", "127.0.0.1:1", "Auth");
        let s = render_json(&records, &meta);
        let v: Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(v["meta"]["filter"], "Auth");
        assert_eq!(v["meta"]["failure_count"], 1);
        assert_eq!(v["meta"]["summary"]["AuthRetryFailed"], 1);
        assert_eq!(v["failures"][0]["kind"], "AuthRetryFailed");
        assert_eq!(v["failures"][0]["request_id"], "r");
        let text = s.to_lowercase();
        assert!(!text.contains("authorization"));
        assert!(!text.contains("refresh_token"));
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
}
