//! Reads the NeoForge oracle's facts.json (tools/neoforge_oracle.py export):
//! exact per-block state properties and per-item components that replace
//! the porter's name-shape heuristics when available.
//!
//! Schema used (mirrors tools/mod_port_harness.py):
//!   blocks: { "<ns:name>": { "states": { "<state-key>": { "hardness": f,
//!             "emission": i, "requires_tool": b, "map_color_id": i, ... } },
//!             "explosion_resistance": f, "friction": f, ... } }
//!   items:  { "<ns:name>": { "max_stack_size": i, "components": { ... },
//!             "enchantment_value": i, "attribute_modifiers": {...}, ... } }
//!   recipes: { "<ns:path>": { "type": "minecraft:smelting", "ingredients":
//!             [[ids]], "result": id, "count": n } }

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct BlockFacts {
    pub hardness: Option<f64>,
    pub explosion_resistance: Option<f64>,
    pub friction: Option<f64>,
    pub light: Option<i64>,
    pub requires_tool: Option<bool>,
    pub map_color: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ItemFacts {
    pub max_stack_size: Option<i64>,
    pub rarity: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub blocks: BTreeMap<String, BlockFacts>,
    pub items: BTreeMap<String, ItemFacts>,
    /// recipe ids the oracle knows -> whether vanilla formats can run it.
    pub recipes: BTreeMap<String, String>,
}

pub fn load(path: &Path) -> Result<Facts> {
    let raw: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;

    let mut facts = Facts::default();

    if let Some(blocks) = raw.get("blocks").and_then(|b| b.as_object()) {
        for (name, b) in blocks {
            let default_state = b
                .get("states")
                .and_then(|s| s.as_object())
                .and_then(|s| {
                    // "" (default) state, or any single state — properties
                    // the harness compares are usually state-uniform.
                    b.get("default")
                        .and_then(|d| d.as_str())
                        .and_then(|d| s.get(d))
                        .or_else(|| s.get(""))
                        .or_else(|| s.values().next())
                });
            let get_f = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_f64());
            let get_i = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_i64());
            facts.blocks.insert(
                name.clone(),
                BlockFacts {
                    hardness: get_f(default_state.map(|s| &s["hardness"])),
                    explosion_resistance: get_f(b.get("explosion_resistance")),
                    friction: get_f(b.get("friction")),
                    light: get_i(default_state.map(|s| &s["emission"])),
                    requires_tool: default_state
                        .map(|s| s["requires_tool"].as_bool())
                        .flatten(),
                    map_color: get_i(default_state.map(|s| &s["map_color_id"])),
                },
            );
        }
    }

    if let Some(items) = raw.get("items").and_then(|i| i.as_object()) {
        for (name, i) in items {
            facts.items.insert(
                name.clone(),
                ItemFacts {
                    max_stack_size: i.get("max_stack_size").and_then(|x| x.as_i64()),
                    rarity: i
                        .get("components")
                        .and_then(|c| c.get("minecraft:rarity"))
                        .and_then(|r| r.as_str())
                        .map(String::from),
                },
            );
        }
    }

    if let Some(recipes) = raw.get("recipes").and_then(|r| r.as_object()) {
        for (id, r) in recipes {
            if let Some(t) = r.get("type").and_then(|t| t.as_str()) {
                facts.recipes.insert(id.clone(), t.to_string());
            }
        }
    }

    Ok(facts)
}

impl Facts {
    /// The fully-qualified oracle id for a mod block/item, given either
    /// namespace from the jar's asset paths.
    pub fn block(&self, ns: &str, name: &str) -> Option<&BlockFacts> {
        self.blocks.get(&format!("{ns}:{name}"))
    }

    pub fn item(&self, ns: &str, name: &str) -> Option<&ItemFacts> {
        self.items.get(&format!("{ns}:{name}"))
    }

    /// Whether a carried recipe file runs in vanilla formats: the oracle
    /// names each recipe's type; anything not minecraft:* needs mod code.
    pub fn recipe_is_vanilla(&self, ns: &str, file: &str) -> Option<bool> {
        // recipe id in oracle: "<ns>:<file-without-.json>" (loader-quoted).
        let id = format!("{ns}:{}", file.trim_end_matches(".json"));
        self.recipes
            .get(&id)
            .map(|t| t.starts_with("minecraft:"))
    }
}
