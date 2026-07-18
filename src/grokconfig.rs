use crate::catalog::{Catalog, normalize_id, supports_fast};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use toml_edit::{DocumentMut, Item, Table, value};

const MANAGED_MARKER: &str = "Managed by grok-build-proxy";

#[derive(Debug)]
pub struct GrokConfig {
    path: PathBuf,
    original: String,
    document: DocumentMut,
}

#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub alias: String,
    pub base_model: String,
    pub effective_model: String,
    pub fast: bool,
    pub name: String,
    pub description: String,
    pub base_url: String,
    pub api_key: String,
    pub context_window: u64,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelRecord {
    pub alias: String,
    pub model: String,
    pub name: String,
    pub base_url: String,
    pub service_tier: String,
    pub managed: bool,
    pub valid: bool,
    pub api_key: String,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvailableModel {
    pub model: String,
    pub name: String,
    pub fast: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStatus {
    pub alias: String,
    pub model: String,
    pub service_tier: String,
    pub configured: bool,
    pub proxy: bool,
    pub ready: bool,
    pub advertised: bool,
    pub metadata: bool,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct ChangeSet {
    pub changes: Vec<String>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
struct EndpointStatus {
    proxy: bool,
    ready: bool,
    models: HashMap<String, JsonValue>,
    detail: String,
}

impl GrokConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let path = resolve_path(path)?;
        let original = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let document = if original.trim().is_empty() {
            DocumentMut::new()
        } else {
            original
                .parse::<DocumentMut>()
                .with_context(|| format!("parse {} as TOML", path.display()))?
        };
        Ok(Self {
            path,
            original,
            document,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn records(&self) -> Vec<ModelRecord> {
        let mut records = Vec::new();
        let Some(models) = self.document.get("model").and_then(Item::as_table_like) else {
            return records;
        };
        for (alias, item) in models.iter() {
            let Some(table) = item.as_table_like() else {
                continue;
            };
            if is_proxy_table(item, table) {
                records.push(record(alias, item, table));
            }
        }
        records.sort_by(|left, right| left.alias.cmp(&right.alias));
        records
    }

    pub fn record(&self, alias: &str) -> Result<ModelRecord> {
        let item = model_item(&self.document, alias)
            .ok_or_else(|| anyhow!("model alias `{alias}` is not configured"))?;
        let table = item
            .as_table_like()
            .ok_or_else(|| anyhow!("model alias `{alias}` is not a TOML table"))?;
        Ok(record(alias, item, table))
    }

    pub fn raw_api_key(&self, alias: &str) -> Result<String> {
        let item = model_item(&self.document, alias)
            .ok_or_else(|| anyhow!("model alias `{alias}` is not configured"))?;
        item.as_table_like()
            .and_then(|table| table.get("api_key"))
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("model alias `{alias}` has no string api_key"))
    }

    pub fn inherited_api_key(&self, base_url: &str) -> Result<Option<String>> {
        let keys: HashSet<_> = self
            .records()
            .into_iter()
            .filter(|record| record.base_url == base_url)
            .filter_map(|record| self.raw_api_key(&record.alias).ok())
            .filter(|key| !key.is_empty() && key != "unused")
            .collect();
        if keys.len() > 1 {
            bail!(
                "proxy models for `{base_url}` use different API keys; set GROK_BUILD_PROXY_TOKEN explicitly"
            )
        }
        Ok(keys.into_iter().next())
    }

    pub fn add(&mut self, spec: &ModelSpec) -> Result<ChangeSet> {
        if model_item(&self.document, &spec.alias).is_some() {
            bail!(
                "model alias `{}` already exists; use `models update`",
                spec.alias
            )
        }
        set_model(&mut self.document, spec, true)?;
        Ok(ChangeSet {
            changes: vec![format!(
                "add {} -> {} ({})",
                spec.alias,
                spec.effective_model,
                tier(spec.fast)
            )],
        })
    }

    pub fn update(&mut self, spec: &ModelSpec) -> Result<ChangeSet> {
        let before = self.record(&spec.alias)?;
        let rendered_before = self.document.to_string();
        let item = model_item(&self.document, &spec.alias).expect("record checked item");
        let table = item.as_table_like().expect("record checked table");
        if !is_proxy_table(item, table) {
            bail!(
                "model alias `{}` is not a proxy-backed model and will not be modified",
                spec.alias
            )
        }
        set_model(&mut self.document, spec, true)?;
        let after = self.record(&spec.alias)?;
        let changes = if rendered_before == self.document.to_string() {
            Vec::new()
        } else {
            vec![format!(
                "update {}: {} ({}) -> {} ({})",
                spec.alias, before.model, before.service_tier, after.model, after.service_tier
            )]
        };
        Ok(ChangeSet { changes })
    }

    pub fn remove(&mut self, alias: &str) -> Result<ChangeSet> {
        let item = model_item(&self.document, alias)
            .ok_or_else(|| anyhow!("model alias `{alias}` is not configured"))?;
        let table = item
            .as_table_like()
            .ok_or_else(|| anyhow!("model alias `{alias}` is not a TOML table"))?;
        if !is_proxy_table(item, table) {
            bail!("model alias `{alias}` is not managed by this proxy")
        }
        self.document["model"]
            .as_table_like_mut()
            .expect("model parent exists")
            .remove(alias);
        Ok(ChangeSet {
            changes: vec![format!("remove {alias}")],
        })
    }

    pub fn sync(&mut self, specs: &[ModelSpec], prune: bool) -> Result<ChangeSet> {
        let mut changes = Vec::new();
        let desired: HashSet<_> = specs.iter().map(|spec| spec.alias.as_str()).collect();
        for spec in specs {
            if let Some(item) = model_item(&self.document, &spec.alias) {
                let table = item
                    .as_table_like()
                    .ok_or_else(|| anyhow!("model alias `{}` is not a TOML table", spec.alias))?;
                if !is_proxy_table(item, table) {
                    bail!(
                        "model alias `{}` conflicts with a non-proxy model",
                        spec.alias
                    )
                }
                if !managed(item) {
                    bail!(
                        "model alias `{}` is proxy-backed but manually managed; use `models update` or remove it before sync",
                        spec.alias
                    )
                }
                let rendered_before = self.document.to_string();
                set_model(&mut self.document, spec, true)?;
                if rendered_before != self.document.to_string() {
                    changes.push(format!(
                        "update {} -> {} ({})",
                        spec.alias,
                        spec.effective_model,
                        tier(spec.fast)
                    ));
                }
            } else {
                set_model(&mut self.document, spec, true)?;
                changes.push(format!(
                    "add {} -> {} ({})",
                    spec.alias,
                    spec.effective_model,
                    tier(spec.fast)
                ));
            }
        }
        if prune {
            let stale: Vec<_> = self
                .records()
                .into_iter()
                .filter(|record| record.managed && !desired.contains(record.alias.as_str()))
                .map(|record| record.alias)
                .collect();
            for alias in stale {
                self.document["model"]
                    .as_table_like_mut()
                    .expect("model parent exists")
                    .remove(&alias);
                changes.push(format!("remove stale managed model {alias}"));
            }
        }
        Ok(ChangeSet { changes })
    }

    pub fn commit(&self) -> Result<Option<PathBuf>> {
        let rendered = self.document.to_string();
        if rendered == self.original {
            return Ok(None);
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let current = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("re-read {}", self.path.display()));
            }
        };
        if current != self.original {
            bail!(
                "{} changed since it was read; refusing to overwrite concurrent edits",
                self.path.display()
            )
        }

        let suffix = uuid::Uuid::new_v4();
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");
        let temp = parent.join(format!(".{file_name}.{suffix}.tmp"));
        let backup =
            (!self.original.is_empty()).then(|| parent.join(format!("{file_name}.bak.{suffix}")));

        let result = (|| -> Result<()> {
            if let Some(backup) = &backup {
                write_new(backup, self.original.as_bytes())?;
            }
            write_new(&temp, rendered.as_bytes())?;
            fs::rename(&temp, &self.path)
                .with_context(|| format!("replace {}", self.path.display()))?;
            set_private_permissions(&self.path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        Ok(backup)
    }
}

pub fn model_spec(
    catalog: &Catalog,
    alias: impl Into<String>,
    target: &str,
    fast_flag: bool,
    listen: &str,
    api_key: &str,
    custom_name: Option<&str>,
) -> Result<ModelSpec> {
    let alias = alias.into();
    validate_alias(&alias)?;
    let (base_model, suffix_fast) = normalize_id(target);
    if base_model.is_empty() {
        bail!("model target cannot be empty")
    }
    if base_model.ends_with("-fast") {
        bail!("model target cannot contain repeated `-fast` suffixes")
    }
    let fast = fast_flag || suffix_fast;
    if fast && !supports_fast(&base_model) {
        bail!("model `{base_model}` does not support the fast priority tier")
    }
    let (model, _) = catalog.lookup(&base_model);
    let name = custom_name.map(str::to_owned).unwrap_or_else(|| {
        format!(
            "Codex {}{}",
            model.display_name,
            if fast { " (Fast)" } else { "" }
        )
    });
    let description = format!(
        "{} via ChatGPT Codex{}",
        model.description.trim_end_matches('.'),
        if fast { " priority tier" } else { "" }
    );
    let effective_model = if fast {
        format!("{base_model}-fast")
    } else {
        base_model.clone()
    };
    let reasoning_efforts = model
        .reasoning
        .map(|capability| {
            capability
                .efforts
                .into_iter()
                .map(|effort| effort.value)
                .collect()
        })
        .unwrap_or_default();
    Ok(ModelSpec {
        alias,
        base_model,
        effective_model,
        fast,
        name,
        description,
        base_url: format!("http://{}/v1", listen.trim_end_matches('/')),
        api_key: if api_key.is_empty() {
            "unused".to_owned()
        } else {
            api_key.to_owned()
        },
        context_window: model.context_window,
        reasoning_efforts,
    })
}

pub fn sync_specs(
    catalog: &Catalog,
    listen: &str,
    api_key: &str,
    include_fast: bool,
) -> Result<(Vec<ModelSpec>, Vec<String>)> {
    let mut specs = Vec::new();
    let mut unsupported = Vec::new();
    let mut aliases = HashSet::new();
    for model in catalog.models() {
        let alias = canonical_alias(&model.id);
        if !aliases.insert(alias.clone()) {
            bail!(
                "catalog model `{}` produces duplicate alias `{alias}`",
                model.id
            )
        }
        specs.push(model_spec(
            catalog, &alias, &model.id, false, listen, api_key, None,
        )?);
        if include_fast {
            if supports_fast(&model.id) {
                let fast_alias = format!("{alias}-fast");
                if !aliases.insert(fast_alias.clone()) {
                    bail!(
                        "catalog model `{}` produces duplicate alias `{fast_alias}`",
                        model.id
                    )
                }
                specs.push(model_spec(
                    catalog, fast_alias, &model.id, true, listen, api_key, None,
                )?);
            } else {
                unsupported.push(model.id);
            }
        }
    }
    Ok((specs, unsupported))
}

pub fn available_models(catalog: &Catalog) -> Vec<AvailableModel> {
    catalog
        .models()
        .into_iter()
        .map(|model| AvailableModel {
            fast: supports_fast(&model.id),
            model: model.id,
            name: model.display_name,
        })
        .collect()
}

pub async fn status(
    config: &GrokConfig,
    alias: Option<&str>,
    expected_listen: &str,
    client_token: &str,
    timeout: Duration,
) -> Result<Vec<ModelStatus>> {
    let records = if let Some(alias) = alias {
        vec![config.record(alias)?]
    } else {
        config.records()
    };
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let mut endpoints = HashMap::new();
    for record in &records {
        let configured_key = config.raw_api_key(&record.alias).unwrap_or_default();
        let token = if client_token.trim().is_empty() && configured_key != "unused" {
            configured_key.as_str()
        } else {
            client_token
        };
        let cache_key = (record.base_url.clone(), token.to_owned());
        if let std::collections::hash_map::Entry::Vacant(entry) = endpoints.entry(cache_key) {
            let result = inspect_endpoint(&client, &record.base_url, token).await;
            entry.insert(result);
        }
    }
    let catalog = Catalog::default();
    let expected_url = format!("http://{}/v1", expected_listen.trim_end_matches('/'));
    Ok(records
        .into_iter()
        .map(|record| {
            let configured_key = config.raw_api_key(&record.alias).unwrap_or_default();
            let token = if client_token.trim().is_empty() && configured_key != "unused" {
                configured_key.as_str()
            } else {
                client_token
            };
            let endpoint = endpoints
                .get(&(record.base_url.clone(), token.to_owned()))
                .cloned()
                .unwrap_or_default();
            let advertised_value = endpoint.models.get(&record.model);
            let advertised = advertised_value.is_some();
            let (base, fast) = normalize_id(&record.model);
            let (catalog_model, _) = catalog.lookup(&base);
            let expected_context = catalog_model.context_window;
            let context_matches = model_item(&config.document, &record.alias)
                .and_then(Item::as_table_like)
                .and_then(|table| table.get("context_window"))
                .and_then(Item::as_integer)
                .is_some_and(|context| context as u64 == expected_context);
            let fast_metadata = !fast
                || advertised_value.is_some_and(|value| {
                    value.get("service_tier").and_then(JsonValue::as_str) == Some("priority")
                        && value.get("target_model").and_then(JsonValue::as_str)
                            == Some(record.model.as_str())
                });
            let metadata = context_matches && (!fast || supports_fast(&base)) && fast_metadata;
            let token_matches =
                client_token.trim().is_empty() || configured_key == client_token.trim();
            let configured = record.valid && record.base_url == expected_url && token_matches;
            let mut details = record.errors;
            if record.base_url != expected_url {
                details.push(format!("endpoint differs from http://{expected_listen}/v1"));
            }
            if !token_matches {
                details.push("configured API key does not match the proxy client token".into());
            }
            if !endpoint.detail.is_empty() {
                details.push(endpoint.detail.clone());
            }
            ModelStatus {
                alias: record.alias,
                model: record.model,
                service_tier: record.service_tier,
                configured,
                proxy: endpoint.proxy,
                ready: endpoint.ready,
                advertised,
                metadata,
                detail: details.join("; "),
            }
        })
        .collect())
}

async fn inspect_endpoint(
    client: &reqwest::Client,
    base_url: &str,
    client_token: &str,
) -> EndpointStatus {
    let root = base_url
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or(base_url.trim_end_matches('/'));
    let health = match client.get(format!("{root}/healthz")).send().await {
        Ok(response) => response.json::<JsonValue>().await.ok(),
        Err(error) => {
            return EndpointStatus {
                detail: format!("proxy unreachable: {error}"),
                ..Default::default()
            };
        }
    };
    let proxy = health
        .as_ref()
        .and_then(|body| body.get("service"))
        .and_then(JsonValue::as_str)
        == Some("grok-build-proxy");
    if !proxy {
        return EndpointStatus {
            detail: "health endpoint is not grok-build-proxy".into(),
            ..Default::default()
        };
    }
    let authorized = |url: String| {
        let request = client.get(url);
        if client_token.trim().is_empty() {
            request
        } else {
            request.bearer_auth(client_token.trim())
        }
    };
    let ready = authorized(format!("{root}/readyz"))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success());
    let models = match authorized(format!("{root}/v1/models")).send().await {
        Ok(response) if response.status().is_success() => response
            .json::<JsonValue>()
            .await
            .ok()
            .and_then(|body| body.get("data").and_then(JsonValue::as_array).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| {
                value
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .map(|id| (id.to_owned(), value.clone()))
            })
            .collect(),
        _ => HashMap::new(),
    };
    EndpointStatus {
        proxy,
        ready,
        models,
        detail: if ready {
            String::new()
        } else {
            "proxy is not ready".into()
        },
    }
}

fn set_model(document: &mut DocumentMut, spec: &ModelSpec, managed: bool) -> Result<()> {
    if !document.contains_key("model") {
        document["model"] = Item::Table(Table::new());
    } else if document["model"].as_table().is_none() {
        document["model"] = document["model"]
            .clone()
            .into_table()
            .map(Item::Table)
            .map_err(|_| anyhow!("`model` must be a TOML table"))?;
    }
    let parent = document["model"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("`model` must be a TOML table"))?;
    if !parent.contains_key(&spec.alias) {
        parent[&spec.alias] = Item::Table(Table::new());
    }
    let table = parent[&spec.alias]
        .as_table_mut()
        .ok_or_else(|| anyhow!("model alias `{}` must be a TOML table", spec.alias))?;
    if managed {
        let prefix = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or("");
        if !prefix.contains(MANAGED_MARKER) {
            table
                .decor_mut()
                .set_prefix(format!("\n# {MANAGED_MARKER}\n"));
        }
    }
    table["model"] = value(&spec.effective_model);
    table["name"] = value(&spec.name);
    table["description"] = value(&spec.description);
    table["base_url"] = value(&spec.base_url);
    table["api_backend"] = value("responses");
    table["api_key"] = value(&spec.api_key);
    table["context_window"] = value(spec.context_window as i64);
    if spec.reasoning_efforts.is_empty() {
        table.remove("supports_reasoning_effort");
        table.remove("reasoning_efforts");
    } else {
        table["supports_reasoning_effort"] = value(true);
        let mut array = toml_edit::Array::new();
        for effort in &spec.reasoning_efforts {
            array.push(effort.as_str());
        }
        table["reasoning_efforts"] = value(array);
    }
    Ok(())
}

fn record(alias: &str, item: &Item, table: &dyn toml_edit::TableLike) -> ModelRecord {
    let mut errors = Vec::new();
    let model = string_field(table, "model", &mut errors);
    let name = string_field(table, "name", &mut errors);
    let base_url = string_field(table, "base_url", &mut errors);
    let backend = string_field(table, "api_backend", &mut errors);
    if !backend.is_empty() && backend != "responses" {
        errors.push("api_backend must be responses".into());
    }
    if table
        .get("context_window")
        .and_then(Item::as_integer)
        .is_none()
    {
        errors.push("context_window must be an integer".into());
    }
    let api_key = match table.get("api_key").and_then(Item::as_str) {
        Some("unused") => "unused",
        Some(value) if !value.is_empty() => "present",
        _ => {
            errors.push("api_key must be a non-empty string".into());
            "missing"
        }
    };
    let (_, fast) = normalize_id(&model);
    ModelRecord {
        alias: alias.to_owned(),
        model,
        name,
        base_url,
        service_tier: tier(fast).to_owned(),
        managed: managed(item),
        valid: errors.is_empty(),
        api_key: api_key.to_owned(),
        errors,
    }
}

fn string_field(table: &dyn toml_edit::TableLike, key: &str, errors: &mut Vec<String>) -> String {
    match table.get(key).and_then(Item::as_str) {
        Some(value) if !value.trim().is_empty() => value.to_owned(),
        _ => {
            errors.push(format!("{key} must be a non-empty string"));
            String::new()
        }
    }
}

fn model_item<'a>(document: &'a DocumentMut, alias: &str) -> Option<&'a Item> {
    document
        .get("model")
        .and_then(Item::as_table_like)
        .and_then(|models| models.get(alias))
}

fn managed(item: &Item) -> bool {
    item.as_table()
        .and_then(|table| table.decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .is_some_and(|prefix| prefix.contains(MANAGED_MARKER))
}

fn is_proxy_table(item: &Item, table: &dyn toml_edit::TableLike) -> bool {
    if managed(item) {
        return true;
    }
    table.get("api_backend").and_then(Item::as_str) == Some("responses")
        && table
            .get("base_url")
            .and_then(Item::as_str)
            .is_some_and(is_loopback_url)
}

fn is_loopback_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn validate_alias(alias: &str) -> Result<()> {
    if alias.trim().is_empty() {
        bail!("model alias cannot be empty")
    }
    if alias.chars().any(char::is_control) {
        bail!("model alias cannot contain control characters")
    }
    Ok(())
}

fn canonical_alias(model: &str) -> String {
    match model {
        "gpt-5.6-sol" => "codex-sol".into(),
        "gpt-5.6-terra" => "codex-terra".into(),
        "gpt-5.6-luna" => "codex-luna".into(),
        _ => format!("codex-{}", model.replace(['.', '_', '/'], "-")),
    }
}

fn tier(fast: bool) -> &'static str {
    if fast { "priority" } else { "standard" }
}

fn resolve_path(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)
            .with_context(|| format!("resolve config symlink {}", path.display())),
        Ok(_) => Ok(path.to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_owned()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn write_new(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    set_private_permissions(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_update_remove_preserve_unrelated_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep\n[models]\ndefault = \"grok-build\"\n\n[model.other]\nmodel = \"other\"\nbase_url = \"https://example.com/v1\"\n",
        )
        .unwrap();
        let catalog = Catalog::default();
        let mut config = GrokConfig::load(&path).unwrap();
        let spec = model_spec(
            &catalog,
            "codex-sol",
            "gpt-5.6-sol",
            false,
            "127.0.0.1:18765",
            "",
            None,
        )
        .unwrap();
        config.add(&spec).unwrap();
        config.commit().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep"));
        assert!(text.contains("[model.other]"));
        assert!(text.contains("# Managed by grok-build-proxy"));

        let mut config = GrokConfig::load(&path).unwrap();
        let fast = model_spec(
            &catalog,
            "codex-sol",
            "gpt-5.6-sol",
            true,
            "127.0.0.1:18765",
            "",
            None,
        )
        .unwrap();
        config.update(&fast).unwrap();
        config.commit().unwrap();
        assert_eq!(
            GrokConfig::load(&path)
                .unwrap()
                .record("codex-sol")
                .unwrap()
                .model,
            "gpt-5.6-sol-fast"
        );

        let mut config = GrokConfig::load(&path).unwrap();
        config.remove("codex-sol").unwrap();
        config.commit().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[model.other]"));
        assert!(!text.contains("[model.codex-sol]"));
    }

    #[test]
    fn sync_fast_is_idempotent_and_prunes_only_managed_models() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[model.manual]\nmodel = \"custom\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 1\nname = \"Manual\"\n",
        )
        .unwrap();
        let catalog = Catalog::new("gpt-5.6-sol,gpt-5.2");
        let (specs, unsupported) = sync_specs(&catalog, "127.0.0.1:18765", "", true).unwrap();
        assert_eq!(unsupported, vec!["gpt-5.2"]);
        let mut config = GrokConfig::load(&path).unwrap();
        config.sync(&specs, false).unwrap();
        config.commit().unwrap();
        let mut config = GrokConfig::load(&path).unwrap();
        assert!(config.sync(&specs, false).unwrap().is_empty());
        config.sync(&specs[..1], true).unwrap();
        config.commit().unwrap();
        let config = GrokConfig::load(&path).unwrap();
        assert!(config.record("manual").is_ok());
        assert!(config.record("codex-sol-fast").is_err());
    }

    #[test]
    fn invalid_toml_and_unsupported_fast_fail_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[broken\n").unwrap();
        assert!(GrokConfig::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[broken\n");
        assert!(
            model_spec(
                &Catalog::default(),
                "codex-old-fast",
                "gpt-5.2",
                true,
                "127.0.0.1:18765",
                "",
                None
            )
            .is_err()
        );
        assert!(
            model_spec(
                &Catalog::default(),
                "codex-double-fast",
                "gpt-5.6-sol-fast-fast",
                false,
                "127.0.0.1:18765",
                "",
                None
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn live_status_accepts_fast_priority_metadata() {
        use axum::{Json, Router, routing::get};
        use serde_json::json;

        let app = Router::new()
            .route(
                "/healthz",
                get(|| async { Json(json!({"service": "grok-build-proxy"})) }),
            )
            .route("/readyz", get(|| async { Json(json!({"ok": true})) }))
            .route(
                "/v1/models",
                get(|| async {
                    Json(json!({"data": [{
                        "id": "gpt-5.6-sol-fast",
                        "service_tier": "priority",
                        "target_model": "gpt-5.6-sol-fast"
                    }]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut config = GrokConfig::load(&path).unwrap();
        let spec = model_spec(
            &Catalog::default(),
            "codex-sol-fast",
            "gpt-5.6-sol",
            true,
            &address.to_string(),
            "",
            None,
        )
        .unwrap();
        config.add(&spec).unwrap();
        let statuses = status(
            &config,
            None,
            &address.to_string(),
            "",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(statuses.len(), 1);
        let status = &statuses[0];
        assert!(status.configured);
        assert!(status.proxy);
        assert!(status.ready);
        assert!(status.advertised);
        assert!(status.metadata);
    }

    #[test]
    fn sync_rejects_manual_alias_collision_and_duplicate_generated_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[model.codex-sol]\nmodel = \"gpt-5.6-sol\"\nname = \"Manual\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 372000\n",
        )
        .unwrap();
        let catalog = Catalog::new("gpt-5.6-sol");
        let (specs, _) = sync_specs(&catalog, "127.0.0.1:18765", "", false).unwrap();
        let mut config = GrokConfig::load(&path).unwrap();
        assert!(config.sync(&specs, false).is_err());
        assert!(
            sync_specs(
                &Catalog::new("foo.bar,foo-bar"),
                "127.0.0.1:18765",
                "",
                false
            )
            .is_err()
        );
    }
}
