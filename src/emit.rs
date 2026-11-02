//! Writes the .hmod source folder: manifest + carried assets/data +
//! generated `data/<id>/horizon/block|item/*.json` declarations, which the
//! jar itself never had (Java mods register blocks in code).
//!
//! Two sources of truth, in order of preference:
//!   1. the NeoForge oracle's facts.json (exact hardness/components/...);
//!   2. name-shape heuristics (ore → diamond_ore, ingot → iron_ingot, ...).
//! The generated file records which source produced it, so a port is
//! auditable — "properties" come from facts when they exist.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::classify::Report;
use crate::facts::Facts;
use crate::jar::JarManifest;

enum Shape {
    /// a vanilla template that matches the block's carried states
    Template(&'static str),
    /// no safe template exists: the block must stay undeclared
    Skip,
    /// nothing stereotyped: fall through to the generic case
    Plain,
}

fn block_template(name: &str) -> Option<serde_json::Value> {
    let (template, props) = match special_shape(name) {
        Shape::Template(t) => (t, serde_json::Value::Null),
        Shape::Skip => return None, // e.g. _chain: no block template in 26.3
        Shape::Plain if name.ends_with("_ore") => ("minecraft:diamond_ore", hardness(3.0)),
        Shape::Plain if name.starts_with("raw_") && name.ends_with("_block") => {
            ("minecraft:raw_iron_block", hardness(5.0))
        }
        Shape::Plain => ("minecraft:iron_block", hardness(5.0)),
    };
    let mut v = serde_json::json!({ "template": template });
    if !props.is_null() {
        v["properties"] = props;
    }
    Some(v)
}

/// Named-shape stereotypes whose blockstates the carried file already
/// describes; the template must match the shape or the loader's state
/// machinery breaks (a ladder forced onto an iron_block template crashed
/// the loader with "invalid climbing facing").
fn special_shape(name: &str) -> Shape {
    // order matters: fence_gate before fence, trapdoor before door
    if name.ends_with("_ladder") {
        Shape::Template("minecraft:ladder")
    } else if name.ends_with("_trapdoor") {
        Shape::Template("minecraft:iron_trapdoor")
    } else if name.ends_with("_fence_gate") {
        Shape::Template("minecraft:oak_fence_gate")
    } else if name.ends_with("_fence") {
        Shape::Template("minecraft:oak_fence")
    } else if name.ends_with("_door") {
        Shape::Template("minecraft:iron_door")
    } else if name.ends_with("_wall") {
        Shape::Template("minecraft:cobblestone_wall")
    } else if name.ends_with("_pane") {
        Shape::Template("minecraft:glass_pane")
    } else if name.ends_with("_bars") {
        Shape::Template("minecraft:iron_bars")
    } else if name.ends_with("_chain") {
        Shape::Skip // minecraft:chain is not a block template in 26.3
    } else if name.ends_with("_glass") {
        Shape::Template("minecraft:glass")
    } else {
        Shape::Plain
    }
}

/// Same as block_template, but overlays exact properties from oracle facts.

/// The variant keys a vanilla template's blockstate expects: what the
/// renderer looks up for each state the template gives the block.
fn template_keys(template: &str) -> Vec<String> {
    match template {
        "minecraft:iron_block" | "minecraft:glass" | "minecraft:amethyst_block"
        | "minecraft:raw_iron_block" | "minecraft:diamond_ore"
        | "minecraft:deepslate_diamond_ore" | "minecraft:iron_bars"
        | "minecraft:iron_trapdoor" | "minecraft:iron_door"
        | "minecraft:oak_fence" | "minecraft:oak_fence_gate"
        | "minecraft:cobblestone_wall" | "minecraft:glass_pane"
        | "minecraft:ladder" => {
            // Templates with facing: 4 keys.
            if template.contains("furnace") || template == "minecraft:ladder" {
                vec!["facing=north".into(), "facing=south".into(), "facing=east".into(), "facing=west".into()]
            } else if template.contains("door") || template.contains("trapdoor") {
                // Doors and trapdoors have facing+half+open — many keys; the
                // renderer accepts any known key, so we give the four facings
                // with the default half/open.
                vec!["facing=north".into(), "facing=south".into(), "facing=east".into(), "facing=west".into()]
            } else {
                vec!["".into()]
            }
        }
        "minecraft:furnace" => {
            let mut keys = Vec::new();
            for facing in ["north", "south", "east", "west"] {
                for lit in ["false", "true"] {
                    keys.push(format!("facing={facing},lit={lit}"));
                }
            }
            keys
        }
        _ => vec!["".into()],
    }
}

/// A normalized blockstate for a block declared with `template`: every key
/// the template's renderer looks up points at the model the original
/// blockstate's first variant (or first multipart apply) used.
fn normalized_blockstate(
    original: Option<&serde_json::Value>,
    template: &str,
    ns: &str,
    base: &str,
) -> serde_json::Value {
    // The model to use: the original's first variant's model, or a
    // fallback cube on the block's own texture.
    let model = original
        .and_then(|bs| {
            bs["variants"].as_object().and_then(|v| v.values().next()).and_then(|entry| {
                // A variant is either a single model object or an array of them.
                let first = entry.as_array().and_then(|a| a.first()).or(Some(entry))?;
                first.get("model").and_then(|m| m.as_str()).map(String::from)
            })
        })
        // Or the first multipart's apply model.
        .or_else(|| {
            original.and_then(|bs| {
                bs["multipart"].as_array().and_then(|m| {
                    m.first().and_then(|part| {
                        part["apply"].as_object().and_then(|a| a["model"].as_str().map(String::from))
                    })
                })
            })
        })
        .unwrap_or_else(|| format!("{ns}:block/{base}"));

    let keys = template_keys(template);
    let mut variants = serde_json::Map::new();
    for key in keys {
        variants.insert(key, serde_json::json!({ "model": model }));
    }
    serde_json::json!({ "variants": variants })
}

fn block_json(name: &str, facts: Option<&crate::facts::BlockFacts>) -> Option<serde_json::Value> {
    let mut v = block_template(name)?;
    if let Some(f) = facts {
        let mut props = v["properties"].take().as_object().cloned().unwrap_or_default();
        if let Some(h) = f.hardness {
            props.insert("hardness".into(), serde_json::json!(h));
        }
        if let Some(e) = f.explosion_resistance {
            props.insert("explosion_resistance".into(), serde_json::json!(e));
        }
        if let Some(l) = f.light {
            props.insert("light".into(), serde_json::json!(l));
        }
        if let Some(fr) = f.friction {
            props.insert("friction".into(), serde_json::json!(fr));
        }
        if let Some(mc) = f.map_color {
            props.insert("map_color".into(), serde_json::json!(mc));
        }
        if let Some(rt) = f.requires_tool {
            props.insert("requires_tool".into(), serde_json::json!(rt));
        }
        v["properties"] = serde_json::Value::Object(props);
    }
    Some(v)
}

fn hardness(h: f64) -> serde_json::Value {
    serde_json::json!({ "hardness": h })
}

fn item_template(name: &str) -> Option<serde_json::Value> {
    let t = if name.starts_with("raw_") {
        "minecraft:raw_iron"
    } else if name.ends_with("_ingot") {
        "minecraft:iron_ingot"
    } else if name.ends_with("_nugget") {
        "minecraft:gold_nugget"
    } else if name.ends_with("_dust") || name.starts_with("dust_") {
        "minecraft:gunpowder"
    } else if name.ends_with("_gem") {
        "minecraft:diamond"
    } else if name.ends_with("_block") || name.ends_with("_ore") {
        // block items are declared by the block file, not here
        return None;
    } else {
        "minecraft:paper"
    };
    Some(serde_json::json!({ "template": t }))
}

fn item_json(name: &str, facts: Option<&crate::facts::ItemFacts>) -> Option<serde_json::Value> {
    let mut v = item_template(name)?;
    if let Some(f) = facts {
        if let Some(stack) = f.max_stack_size {
            v["max_stack_size"] = serde_json::json!(stack);
        }
        if let Some(r) = &f.rarity {
            v["rarity"] = serde_json::json!(r);
        }
    }
    Some(v)
}

pub struct GenOutcome {
    pub declared_blocks: usize,
    pub declared_items: usize,
    pub skipped: Vec<String>,
    /// blocks that got oracle-exact properties rather than heuristics.
    pub exact_blocks: usize,
    pub exact_items: usize,
    /// loot tables generated for blocks that shipped none in the jar.
    pub generated_loot: usize,
}

/// Sanitises a mod id to Horizon's [a-z0-9_]{1,64}.
pub fn sanitize_id(id: &str) -> String {
    let mut s: String = id
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    if s.is_empty() {
        s.push('m');
    }
    s.truncate(64);
    s
}

pub fn write_hmod(
    out: &Path,
    manifest: &JarManifest,
    report: &Report,
    facts: &Facts,
    carry: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<GenOutcome> {
    let id = sanitize_id(&manifest.mod_id);
    fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;

    // namespaces the mod claims: only those its CARRIED files actually use.
    // Companion-library namespaces (curios & co.) never port and must not be
    // declared, or two ports of mods that both use them collide in the game.
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    for name in report.carry_files.keys().chain(report.carry_assets.keys()) {
        if let Some(ns) = name.split('/').nth(1) {
            // minecraft/c/horizon can't be claimed (Horizon refuses); tags done
            // under minecraft/tags are the only exception and need no listing.
            if !ns.is_empty() && ns != id && !matches!(ns, "minecraft" | "c" | "horizon") {
                namespaces.insert(ns.to_string());
            }
        }
    }
    let namespaces: Vec<String> = namespaces.into_iter().collect();

    // manifest: only the keys Horizon's schema allows.
    let m = toml::Table::from_iter([
        ("id".to_string(), toml::Value::String(id.clone())),
        ("name".to_string(), toml::Value::String(manifest.display_name.clone().unwrap_or_else(|| id.clone()))),
        ("version".to_string(), toml::Value::String(manifest.version.clone().unwrap_or_else(|| "0.1.0".into()))),
        ("description".to_string(), toml::Value::String(manifest.description.clone().unwrap_or_else(|| format!("Data-only port of {} ({} loader), material/content layer only.", manifest.mod_id, manifest.loader)))),
        ("authors".to_string(), toml::Value::Array(manifest.authors.iter().map(|a| toml::Value::String(a.clone())).collect())),
        ("api".to_string(), toml::Value::String("0.2".into())),
        ("side".to_string(), toml::Value::String("both".into())),
        ("capabilities".to_string(), toml::Value::Array(vec![toml::Value::String("registries".into())])),
    ]);
    let mut m = m;
    if !namespaces.is_empty() {
        m.insert("namespaces".into(), toml::Value::Array(namespaces.iter().map(|n| toml::Value::String(n.clone())).collect()));
    }
    fs::write(out.join("horizon-mod.toml"), toml::to_string(&m)?)?;

    // carried files, at their original paths. Recipes convert: the common
    // datagen shapes (item_input/main_output/extra_input families, learned
    // porting Mekanism) rewrite into the custom-recipe format the engine's
    // machine hooks read; fluid/gas-only recipes drop with a reason.
    let mut recipes_converted = 0usize;
    let mut recipes_skipped = 0usize;
    for name in report.carry_files.keys().chain(report.carry_assets.keys()) {
        let Some(bytes) = carry(name) else { continue };
        let dest = out.join(name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let is_recipe = name.starts_with("data/")
            && name.split('/').nth(2) == Some("recipe")
            && name.ends_with(".json");
        if is_recipe {
            let jar_ns = name.split('/').nth(1).unwrap_or("");
            match serde_json::from_slice::<serde_json::Value>(bytes.as_slice()) {
                Ok(recipe) => match crate::recipes::convert(&recipe, jar_ns, &id) {
                    crate::recipes::Outcome::Converted(v) => {
                        fs::write(&dest, serde_json::to_vec_pretty(&v)?)?;
                        recipes_converted += 1;
                        continue;
                    }
                    crate::recipes::Outcome::Skipped(why) => {
                        println!("  recipe {}: skipped ({})", name.rsplit('/').next().unwrap_or(name), why);
                        recipes_skipped += 1;
                        continue;
                    }
                    crate::recipes::Outcome::Carried => {}
                },
                Err(_) => {} // unparsable recipe carries as-is; the packer logs it
            }
        }
        fs::write(dest, bytes)?;
    }
    if recipes_converted > 0 || recipes_skipped > 0 {
        println!("recipes: {} converted, {} skipped", recipes_converted, recipes_skipped);
    }

    // derive block/item declarations from the asset tree.
    let mut declared_blocks = 0;
    let mut declared_items = 0;
    let mut exact_blocks = 0;
    let mut exact_items = 0;
    let mut generated_loot = 0;
    let mut skipped = Vec::new();

    let mut blocks: BTreeSet<(String, String)> = BTreeSet::new();
    let mut items: BTreeSet<(String, String)> = BTreeSet::new();
    for name in report.carry_assets.keys() {
        let parts: Vec<&str> = name.splitn(3, '/').collect();
        if parts.len() < 3 {
            continue;
        }
        let (ns, rest) = (parts[1], parts[2]);
        if rest.starts_with("blockstates/") && rest.ends_with(".json") {
            let base = rest.trim_start_matches("blockstates/").trim_end_matches(".json");
            // only declare blocks whose blockstate carried: a declared block
            // without its states file re-enters the loader's crash path.
            blocks.insert((ns.into(), base.into()));
        }
        if (rest.starts_with("models/item/") || rest.starts_with("items/")) && rest.ends_with(".json") {
            let base = rest.split('/').last().unwrap().trim_end_matches(".json");
            if !blocks.contains(&(ns.into(), base.into())) {
                items.insert((ns.into(), base.into()));
            }
        }
    }

    for (ns, base) in &blocks {
        let bf = facts.block(ns, base);
        match block_json(base, bf) {
            Some(json) => {
                let p = out.join(format!("data/{id}/horizon/block/{base}.json"));
                fs::create_dir_all(p.parent().unwrap())?;
                fs::write(p, serde_json::to_vec_pretty(&json)?)?;
                declared_blocks += 1;
                if bf.is_some() {
                    exact_blocks += 1;
                }

                // Normalize the blockstate to the template's states: the
                // original's variant keys (facing=..., active=...) don't
                // match what the template's renderer looks up, which left
                // 37% of ported blocks as missing-texture checkerboards.
                let bs_path = format!("assets/{ns}/blockstates/{base}.json");
                let original: Option<serde_json::Value> = report
                    .carry_assets
                    .get(&bs_path)
                    .and_then(|e| match e {
                        crate::jar::Entry::Resource(b) => serde_json::from_slice(b).ok(),
                        _ => None,
                    });
                let template = json["template"].as_str().unwrap_or("minecraft:iron_block");
                let normalized = normalized_blockstate(original.as_ref(), template, ns, base);
                let p = out.join(&bs_path);
                fs::create_dir_all(p.parent().unwrap())?;
                fs::write(p, serde_json::to_vec_pretty(&normalized)?)?;
            }
            None => skipped.push(format!("{ns}:{base} (block)")),
        }

        // blocks the jar shipped no loot table for get a drop-self table,
        // so the port never gives a block that vanishes when broken.
        let loot_path = format!("data/{ns}/loot_table/blocks/{base}.json");
        if !report.carry_files.contains_key(&loot_path) {
            let p = out.join(&loot_path);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(
                &p,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "type": "minecraft:block",
                    "pools": [{ "rolls": 1.0, "bonus_rolls": 0,
                        "entries": [{ "type": "minecraft:item",
                                      "name": format!("{ns}:{base}") }] }]
                }))?,
            )?;
            generated_loot += 1;
        }
    }
    for (ns, base) in &items {
        let f = facts.item(ns, base);
        match item_json(base, f) {
            Some(json) => {
                let p = out.join(format!("data/{id}/horizon/item/{base}.json"));
                fs::create_dir_all(p.parent().unwrap())?;
                fs::write(p, serde_json::to_vec_pretty(&json)?)?;
                declared_items += 1;
                if f.is_some() {
                    exact_items += 1;
                }
                // 26.3 item model definitions (assets/<ns>/items/): absent in
                // 1.21.1 jars, without them items draw the missing-texture
                // checkerboard. Point each at the carried models/item file.
                let p = out.join(format!("assets/{ns}/items/{base}.json"));
                if !report.carry_assets.contains_key(&format!("assets/{ns}/items/{base}.json")) {
                    fs::create_dir_all(p.parent().unwrap())?;
                    fs::write(
                        &p,
                        serde_json::to_vec_pretty(&serde_json::json!({
                            "model": {
                                "type": "minecraft:model",
                                "model": format!("{ns}:item/{base}")
                            }
                        }))?,
                    )?;
                }
            }
            None => skipped.push(format!("{ns}:{base} (item)")),
        }
    }

    // Block items need a definition too (drawn in inventory/hotbar): point
    // each at the carried models/block file through the blockstate's model.
    for (ns, base) in &blocks {
        // A block whose item model the jar never shipped (mod containers,
        // machines) gets a parent link to its block model, and a missing
        // block model a cube fallback on the carried top texture, so the
        // block is never invisible or checkerboard.
        let item_model_path = format!("assets/{ns}/models/item/{base}.json");
        let block_model_path = format!("assets/{ns}/models/block/{base}.json");
        let has_item_model = report.carry_assets.contains_key(&item_model_path);
        let has_block_model = report.carry_assets.contains_key(&block_model_path);

        if !has_block_model {
            // Cube fallback on any carried block texture of the name, or the
            // generic <ns> first block texture if none matches.
            let tex = format!("assets/{ns}/textures/block/{base}.png");
            let all: Vec<String> = report
                .carry_assets
                .keys()
                .filter(|k| k.starts_with(&format!("assets/{ns}/textures/block/")))
                .cloned()
                .collect();
            let side = if report.carry_assets.contains_key(&tex) {
                format!("{ns}:block/{base}")
            } else if let Some(first) = all.first() {
                let name = first.trim_start_matches(&format!("assets/{ns}/textures/block/"))
                    .trim_end_matches(".png");
                format!("{ns}:block/{name}")
            } else {
                continue // no textures at all: nothing to draw anyway
            };
            let p = out.join(&block_model_path);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(
                &p,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "parent": "minecraft:block/cube_all",
                    "textures": { "all": side }
                }))?,
            )?;
        }

        if !has_item_model {
            let p = out.join(&item_model_path);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(
                &p,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "parent": format!("{ns}:block/{base}")
                }))?,
            )?;
        }

        let p = out.join(format!("assets/{ns}/items/{base}.json"));
        if p.exists()
            || report.carry_assets.contains_key(&format!("assets/{ns}/items/{base}.json"))
        {
            continue;
        }
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(
            &p,
            serde_json::to_vec_pretty(&serde_json::json!({
                "model": {
                    "type": "minecraft:model",
                    "model": format!("{ns}:item/{base}")
                }
            }))?,
        )?;
    }

    Ok(GenOutcome {
        declared_blocks,
        declared_items,
        skipped,
        exact_blocks,
        exact_items,
        generated_loot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_id_lowercase_and_filtered() {
        assert_eq!(sanitize_id("UsefulBackpacks"), "usefulbackpacks");
        let s = sanitize_id("Some Weird ID!!");
        assert!(!s.is_empty());
        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
    }

    #[test]
    fn sanitize_id_64_chars_max() {
        assert_eq!(sanitize_id(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn ore_gets_diamond_ore_template() {
        let v = block_template("zinc_ore").unwrap();
        assert_eq!(v["template"], "minecraft:diamond_ore");
        assert_eq!(v["properties"]["hardness"], 3.0);
    }

    #[test]
    fn raw_block_gets_raw_iron_block() {
        let v = block_template("raw_zinc_block").unwrap();
        assert_eq!(v["template"], "minecraft:raw_iron_block");
    }

    #[test]
    fn generic_block_gets_iron_block() {
        let v = block_template("some_machine_frame").unwrap();
        assert_eq!(v["template"], "minecraft:iron_block");
    }

    #[test]
    fn ingot_item_template() {
        assert_eq!(item_template("tin_ingot").unwrap()["template"], "minecraft:iron_ingot");
        assert_eq!(item_template("raw_tin").unwrap()["template"], "minecraft:raw_iron");
        assert_eq!(item_template("dust_tin").unwrap()["template"], "minecraft:gunpowder");
    }

    #[test]
    fn block_items_not_declared_as_items() {
        assert!(item_template("some_block").is_none());
        assert!(item_template("copper_ore").is_none());
    }

    #[test]
    fn facts_override_heuristics() {
        let bf = crate::facts::BlockFacts {
            hardness: Some(7.2),
            explosion_resistance: Some(9.1),
            friction: Some(0.7),
            light: Some(5),
            requires_tool: Some(true),
            map_color: Some(41),
        };
        let v = block_json("zinc_ore", Some(&bf)).unwrap();
        assert_eq!(v["properties"]["hardness"], 7.2);
        assert_eq!(v["properties"]["explosion_resistance"], 9.1);
        assert_eq!(v["properties"]["light"], 5);
        assert_eq!(v["properties"]["requires_tool"], true);
        assert_eq!(v["properties"]["map_color"], 41);
        // template still chosen by heuristic
        assert_eq!(v["template"], "minecraft:diamond_ore");
    }

    #[test]
    fn item_facts_set_stack_and_rarity() {
        let f = crate::facts::ItemFacts { max_stack_size: Some(16), rarity: Some("rare".into()) };
        let v = item_json("weird_orb", Some(&f)).unwrap();
        assert_eq!(v["max_stack_size"], 16);
        assert_eq!(v["rarity"], "rare");
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn ladder_gets_ladder_template_not_iron() {
        let v = block_template("dark_steel_ladder").unwrap();
        assert_eq!(v["template"], "minecraft:ladder");
    }

    #[test]
    fn chain_is_never_a_template() {
        // minecraft:chain is not a block in 26.3: a declared block with that
        // template refuses the whole mod's content in the loader.
        assert!(block_template("soul_chain").is_none());
    }

    #[test]
    fn fence_and_door_stereotypes() {
        assert_eq!(block_template("painted_fence").unwrap()["template"], "minecraft:oak_fence");
        assert_eq!(block_template("dark_steel_door").unwrap()["template"], "minecraft:iron_door");
    }
}
