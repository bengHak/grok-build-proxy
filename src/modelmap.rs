use crate::catalog::normalize_id;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    pub target: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub requested: String,
    pub model: String,
    pub fast: bool,
    pub mapped: bool,
    pub chain: Vec<String>,
}
impl Resolution {
    pub fn effective_model_id(&self) -> String {
        if self.fast && !self.model.is_empty() {
            format!("{}-fast", self.model)
        } else {
            self.model.clone()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelMap {
    direct: HashMap<String, String>,
    entries: Vec<Entry>,
}
impl ModelMap {
    pub fn parse(value: &str) -> Result<Self> {
        let mut direct = HashMap::new();
        let mut entries = Vec::new();
        for raw in value
            .split([',', ';', '\n', '\r'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let Some((source, target)) = raw.split_once('=') else {
                bail!("invalid model substitution {raw:?}: expected source=target")
            };
            let (source, target) = (source.trim(), target.trim());
            if source.is_empty() || target.is_empty() {
                bail!("invalid model substitution {raw:?}: source and target are required")
            }
            if target.contains('=') {
                bail!("invalid model substitution {raw:?}: too many '=' separators")
            }
            if source.chars().any(|c| c.is_whitespace() || c.is_control())
                || target.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                bail!("model IDs cannot contain whitespace or control characters")
            }
            if source == target {
                bail!("invalid model substitution {raw:?}: source and target are identical")
            }
            if direct.insert(source.into(), target.into()).is_some() {
                bail!("duplicate model substitution for {source:?}")
            }
            entries.push(Entry {
                source: source.into(),
                target: target.into(),
            });
        }
        let map = Self { direct, entries };
        for e in &map.entries {
            map.try_resolve(&e.source)?;
        }
        Ok(map)
    }
    fn try_resolve(&self, requested: &str) -> Result<Resolution> {
        let requested = requested.trim().to_string();
        let (base_requested, mut fast) = normalize_id(&requested);
        if base_requested.is_empty() {
            return Ok(Resolution {
                requested,
                model: String::new(),
                fast: false,
                mapped: false,
                chain: vec![],
            });
        }
        let mut current = requested.clone();
        let mut chain = vec![requested.clone()];
        let mut visited = HashSet::new();
        loop {
            let (base, has_fast) = normalize_id(&current);
            fast |= has_fast;
            let (lookup, next) = if let Some(next) = self.direct.get(&current) {
                (current.clone(), Some(next))
            } else {
                (base.clone(), self.direct.get(&base))
            };
            let Some(next) = next else {
                current = base;
                break;
            };
            if !visited.insert(lookup.clone()) {
                chain.push(lookup);
                bail!("model substitution cycle: {}", chain.join(" -> "))
            }
            current = next.trim().into();
            chain.push(current.clone());
        }
        let (model, target_fast) = normalize_id(&current);
        fast |= target_fast;
        Ok(Resolution {
            requested,
            mapped: chain.len() > 1 || model != base_requested,
            model,
            fast,
            chain,
        })
    }
    pub fn resolve(&self, requested: &str) -> Resolution {
        self.try_resolve(requested).unwrap_or_else(|_| {
            let (model, fast) = normalize_id(requested);
            Resolution {
                requested: requested.trim().into(),
                model,
                fast,
                mapped: false,
                chain: vec![requested.trim().into()],
            }
        })
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.direct.len()
    }
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty()
    }
    pub fn stable_string(&self) -> String {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.source.cmp(&b.source));
        entries
            .into_iter()
            .map(|e| format!("{}={}", e.source, e.target))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chains_and_fast() {
        let m = ModelMap::parse("alias=middle,middle=gpt-5.6-sol-fast").unwrap();
        let r = m.resolve("alias");
        assert_eq!(r.model, "gpt-5.6-sol");
        assert!(r.fast && r.mapped);
    }
    #[test]
    fn rejects_cycles() {
        assert!(ModelMap::parse("a=b,b=a").is_err());
    }
}
