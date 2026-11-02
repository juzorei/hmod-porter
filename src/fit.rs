//! The fit layer: reconcile what a port CARRIED with what its DECLARATIONS
//! promise. Every rule here comes from a bug the Mekanism port actually hit
//! in game; each one is generic, not mod-specific.
//!
//! Rules (in the order they run):
//! 1. **Template/blockstate property match.** A block declared with the
//!    property-less `minecraft:iron_block` template cannot be looked up by
//!    the blockstate's `facing=north,lit=false` keys — the engine finds no
//!    variant and draws the missing-texture checkerboard. Measured on the
//!    Mekanism port: 50 blocks (every tiered transmitter, every multiblock
//!    casing, creative blocks). Fix: keep one property-less variant carrying
//!    the block's model.
//! 2. **Orphan parents.** A model whose `parent` names a model that was not
//!    carried renders as nothing (invisible block) or as the checkerboard.
//!    Fix: report it, and substitute the module's base `machine` model when
//!    the parent is clearly a Mekanism-style module base.
//! 3. **Texture references.** Every `mekanism_materials:block/<path>` a model
//!    names must have a PNG; a missing one is a checkerboard face.
//!
//! The layer never invents geometry: it only rewrites keys the declarations
//! cannot satisfy, and it reports everything it touched (the CLI prints the
//! summary, so a human sees what the port could not reproduce faithfully).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// What the fit pass changed, for the CLI summary.
#[derive(Default, Debug)]
pub struct FitReport {
    /// Blocks whose blockstate was collapsed to one property-less variant.
    pub collapsed: Vec<String>,
    /// Models whose parent model is missing (path -> parent).
    pub orphan_parents: Vec<(String, String)>,
    /// Models naming a texture that was not carried (model -> texture).
    pub missing_textures: Vec<(String, String)>,
    /// `neoforge:composite` models flattened into plain models (Horizon has
    /// no loader support: a composite renders as NOTHING — the invisible
    /// block class).
    pub flattened: Vec<String>,
}

impl FitReport {
    pub fn is_clean(&self) -> bool {
        self.collapsed.is_empty()
            && self.orphan_parents.is_empty()
            && self.missing_textures.is_empty()
            && self.flattened.is_empty()

    }
    pub fn summary(&self) -> String {
        format!(
            "fit: {} composite(s) flattened, {} blockstate(s) collapsed, {} orphan parent(s), {} missing texture(s)",
            self.flattened.len(),
            self.collapsed.len(),
            self.orphan_parents.len(),
            self.missing_textures.len()
        )
    }
}

/// Reconciles the emitted pack at `out` (a horizon-mod source tree).
pub fn run(out: &Path, mod_id: &str) -> Result<FitReport> {
    let mut report = FitReport::default();
    let assets = out.join("assets").join(mod_id);
    let blockstates = assets.join("blockstates");
    let models = assets.join("models").join("block");
    let textures = assets.join("textures").join("block");

    // Which properties each vanilla template offers. Horizon resolves a
    // blockstate by the block's real state; a template's state keys are the
    // ones its vanilla blockstate declares.
    let template_props = template_properties();

    // ---- rule 1: template/blockstate property match -----------------------
    if blockstates.is_dir() {
        let mut decls: BTreeMap<String, String> = BTreeMap::new();
        let block_dir = out
            .join("data")
            .join(mod_id)
            .join("horizon")
            .join("block");
        if block_dir.is_dir() {
            for entry in fs::read_dir(&block_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Ok(text) = fs::read_to_string(&path)
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
                    && let Some(t) = json.get("template").and_then(|v| v.as_str())
                {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
                    decls.insert(name, t.to_owned());
                }
            }
        }
        for entry in fs::read_dir(&blockstates)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let block = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            };
            let Some(template) = decls.get(&block) else { continue };
            let Some(allowed) = template_props.get(template) else { continue };
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            let Some(variants) = json.get("variants").and_then(|v| v.as_object()) else { continue };
            // keys this blockstate uses that the template does not define
            let mut phantom = false;
            for key in variants.keys() {
                for kv in key.split(',') {
                    if kv.is_empty() {
                        continue;
                    }
                    let Some((prop, _)) = kv.split_once('=') else { continue };
                    if !allowed.contains(prop) {
                        phantom = true;
                    }
                }
            }
            if !phantom {
                continue;
            }
            // keep the first model, drop every property key
            let first = variants.values().next().cloned();
            if let (Some(model), Some(obj)) = (first, json.as_object_mut()) {
                let mut one = serde_json::Map::new();
                one.insert(String::new(), model);
                obj.insert("variants".into(), serde_json::Value::Object(one));
                fs::write(&path, serde_json::to_string_pretty(&json)? + "\n")?;
                report.collapsed.push(block);
            }
        }
    }

    // ---- rule 1b: flatten neoforge:composite models -----------------------
    // A composite model (`{"loader":"neoforge:composite","children":{...}}`)
    // is a NeoForge loader feature. Horizon has no loaders: it reads the
    // model, finds no `elements` and no usable `parent`, and draws NOTHING —
    // the block is invisible and the floor shows through. Measured on the
    // Mekanism port: 66 models (all 32 factories, energy cubes, multiblock
    // ports). Fix: merge the children into one flat model.
    if models.is_dir() {
        let mut stack = vec![models.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries: Vec<PathBuf> = Vec::new();
            for e in fs::read_dir(&dir)? {
                entries.push(e?.path());
            }
            for path in entries {
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else { continue };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                if json.get("loader").is_none() && json.get("children").is_none() {
                    continue;
                }
                let rel = path
                    .strip_prefix(&models)
                    .ok()
                    .and_then(|r| r.with_extension("").to_str().map(|s| s.replace('\\', "/")))
                    .unwrap_or_default();
                let flat = flatten_model(&models, &rel, 0);
                if flat.get("elements").and_then(|e| e.as_array()).is_none_or(|a| a.is_empty()) {
                    continue; // nothing to merge — leave it for the report
                }
                fs::write(&path, serde_json::to_string_pretty(&flat)? + "
")?;
                report.flattened.push(rel);
            }
        }
    }

    // ---- rules 2 and 3: model parents and textures ------------------------
    if models.is_dir() {
        let mut have: BTreeSet<String> = BTreeSet::new();
        let mut all: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![models.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "json") {
                    let rel = p
                        .strip_prefix(&models)
                        .ok()
                        .and_then(|r| r.with_extension("").to_str().map(|s| s.replace('\\', "/")))
                        .unwrap_or_default();
                    all.insert(rel);
                }
            }
        }
        have = all.clone();

        let prefix = format!("{mod_id}:block/");
        for name in &all {
            let p = models.join(format!("{name}.json"));
            let Ok(text) = fs::read_to_string(&p) else { continue };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            // rule 2: parent must exist (or be a vanilla/loader parent)
            if let Some(parent) = json.get("parent").and_then(|v| v.as_str())
                && let Some(rest) = parent.strip_prefix(&prefix)
                && !have.contains(rest)
            {
                report.orphan_parents.push((name.clone(), parent.to_owned()));
            }
            // rule 3: every texture file must exist
            if let Some(texs) = json.get("textures").and_then(|v| v.as_object()) {
                for (_, v) in texs {
                    let Some(src) = v.as_str() else { continue };
                    if src.starts_with('#') {
                        continue;
                    }
                    let Some(rest) = src.strip_prefix(&prefix) else { continue };
                    let file = textures.join(format!("{rest}.png"));
                    if !file.exists() {
                        report.missing_textures.push((name.clone(), rest.to_owned()));
                    }
                }
            }
        }
    }

    Ok(report)
}

/// Merges a `neoforge:composite` model (and its children's parent chains)
/// into one plain model: elements concatenated, textures merged, parent
/// `block/block`. Depth-limited so a cyclic parent cannot hang the port.
fn flatten_model(models: &Path, name: &str, depth: usize) -> serde_json::Value {
    let mut elements: Vec<serde_json::Value> = Vec::new();
    let mut textures = serde_json::Map::new();
    if depth > 6 {
        return serde_json::json!({"parent": "block/block", "textures": {}, "elements": []});
    }
    let path = models.join(format!("{name}.json"));
    let Ok(text) = fs::read_to_string(&path) else {
        return serde_json::json!({"parent": "block/block", "textures": {}, "elements": []});
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return serde_json::json!({"parent": "block/block", "textures": {}, "elements": []});
    };
    // children first (a composite's own elements are usually absent)
    if let Some(children) = json.get("children").and_then(|c| c.as_object()) {
        for child in children.values() {
            let Some(parent) = child.get("parent").and_then(|p| p.as_str()) else { continue };
            let Some(rest) = parent.split("block/").nth(1) else { continue };
            let sub = flatten_model(models, rest, depth + 1);
            if let Some(els) = sub.get("elements").and_then(|e| e.as_array()) {
                elements.extend(els.iter().cloned());
            }
            if let Some(texs) = sub.get("textures").and_then(|t| t.as_object()) {
                for (k, v) in texs {
                    textures.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
    }
    if let Some(els) = json.get("elements").and_then(|e| e.as_array()) {
        elements.extend(els.iter().cloned());
    }
    if let Some(texs) = json.get("textures").and_then(|t| t.as_object()) {
        for (k, v) in texs {
            textures.insert(k.clone(), v.clone());
        }
    }
    // a plain parent chain contributes its elements too
    if let Some(parent) = json.get("parent").and_then(|p| p.as_str())
        && let Some(rest) = parent.split("block/").nth(1)
        && !rest.starts_with("block")
    {
        let sub = flatten_model(models, rest, depth + 1);
        if let Some(els) = sub.get("elements").and_then(|e| e.as_array()) {
            elements.extend(els.iter().cloned());
        }
        if let Some(texs) = sub.get("textures").and_then(|t| t.as_object()) {
            for (k, v) in texs {
                textures.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    serde_json::json!({
        "parent": "block/block",
        "textures": serde_json::Value::Object(textures),
        "elements": serde_json::Value::Array(elements),
    })
}

/// The state properties each vanilla template's block declares.
fn template_properties() -> BTreeMap<String, BTreeSet<String>> {
    let mut map = BTreeMap::new();
    // Horizon's template blocks and their state keys. Only the properties a
    // mod would trip over matter; a template absent from this table is
    // skipped (rule 1 does not guess).
    let table: [(&str, &[&str]); 14] = [
        ("minecraft:iron_block", &[]),
        ("minecraft:furnace", &["facing", "lit"]),
        ("minecraft:blast_furnace", &["facing", "lit"]),
        ("minecraft:smoker", &["facing", "lit"]),
        ("minecraft:chest", &["facing", "type", "waterlogged"]),
        ("minecraft:barrel", &["facing", "open"]),
        ("minecraft:hopper", &["enabled", "facing"]),
        ("minecraft:dispenser", &["facing", "triggered"]),
        ("minecraft:dropper", &["facing", "triggered"]),
        ("minecraft:piston", &["extended", "facing"]),
        ("minecraft:observer", &["facing", "powered"]),
        ("minecraft:iron_bars", &["east", "north", "south", "waterlogged", "west"]),
        ("minecraft:glass", &[]),
        ("minecraft:oak_log", &["axis"]),
    ];
    for (name, props) in table {
        map.insert(
            name.to_owned(),
            props.iter().map(|p| (*p).to_owned()).collect::<BTreeSet<_>>(),
        );
    }
    map
}

/// Rewrites an orphan parent to the module's base model when the name looks
/// like a Mekanism module base (`.../base`, `machine`); returns how many.
pub fn repair_orphan_parents(out: &Path, mod_id: &str, orphans: &[(String, String)]) -> Result<usize> {
    let models: PathBuf = out.join("assets").join(mod_id).join("models").join("block");
    let mut fixed = 0;
    for (name, parent) in orphans {
        let tail = parent.rsplit('/').next().unwrap_or("");
        let fallback = if tail == "base" || tail.starts_with("base_") {
            format!("{mod_id}:block/machine")
        } else {
            continue;
        };
        let p = models.join(format!("{name}.json"));
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(obj) = json.as_object_mut() {
            obj.insert("parent".into(), serde_json::Value::String(fallback));
            fs::write(&p, serde_json::to_string_pretty(&json)? + "\n")?;
            fixed += 1;
        }
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iron_block_template_allows_no_properties() {
        let table = template_properties();
        let iron = table.get("minecraft:iron_block").unwrap();
        assert!(iron.is_empty());
        let furnace = table.get("minecraft:furnace").unwrap();
        assert!(furnace.contains("facing") && furnace.contains("lit"));
    }

    #[test]
    fn report_starts_clean() {
        let r = FitReport::default();
        assert!(r.is_clean());
        assert!(r.summary().starts_with("fit: 0 composite(s) flattened"));
    }
}

#[cfg(test)]
mod template_match_tests {
    use super::*;
    use std::fs;

    /// The bug the rule exists for: a block declared on the property-less
    /// `minecraft:iron_block` template, whose blockstate still keys on
    /// `facing`/`lit`. Before the fix the engine found no variant and drew
    /// the checkerboard; after it the blockstate collapses to one variant.
    #[test]
    fn phantom_properties_collapse_to_one_variant() {
        let dir = std::env::temp_dir().join("hmod-porter-fit-test");
        let _ = fs::remove_dir_all(&dir);
        let assets = dir.join("assets").join("mm");
        let blocks = dir.join("data").join("mm").join("horizon").join("block");
        fs::create_dir_all(assets.join("blockstates")).unwrap();
        fs::create_dir_all(&blocks).unwrap();
        fs::create_dir_all(assets.join("models").join("block")).unwrap();
        fs::create_dir_all(assets.join("textures").join("block")).unwrap();

        fs::write(
            blocks.join("crusher.json"),
            r#"{"template":"minecraft:iron_block"}"#,
        )
        .unwrap();
        fs::write(
            assets.join("blockstates").join("crusher.json"),
            r#"{"variants":{"facing=north,lit=false":{"model":"mm:block/crusher"},"facing=south,lit=true":{"model":"mm:block/crusher"}}}"#,
        )
        .unwrap();
        fs::write(
            assets.join("models").join("block").join("crusher.json"),
            r#"{"parent":"mm:block/machine","textures":{"sides":"mm:block/steel"}}"#,
        )
        .unwrap();
        fs::write(assets.join("textures").join("block").join("steel.png"), b"x").unwrap();

        let report = run(&dir, "mm").unwrap();
        assert_eq!(report.collapsed, vec!["crusher".to_string()]);

        let text = fs::read_to_string(assets.join("blockstates").join("crusher.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let variants = json["variants"].as_object().unwrap();
        assert_eq!(variants.len(), 1);
        assert!(variants.contains_key(""));
        assert_eq!(variants[""]["model"], "mm:block/crusher");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_texture_is_reported() {
        let dir = std::env::temp_dir().join("hmod-porter-fit-tex");
        let _ = fs::remove_dir_all(&dir);
        let assets = dir.join("assets").join("mm");
        fs::create_dir_all(assets.join("models").join("block")).unwrap();
        fs::write(
            assets.join("models").join("block").join("m.json"),
            r#"{"textures":{"all":"mm:block/nope"}}"#,
        )
        .unwrap();
        let report = run(&dir, "mm").unwrap();
        assert_eq!(report.missing_textures.len(), 1);
        assert_eq!(report.missing_textures[0].1, "nope");
        let _ = fs::remove_dir_all(&dir);
    }
}
