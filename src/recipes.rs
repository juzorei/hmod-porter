//! Modded-recipe conversion: recognizes the common datagen recipe SHAPES
//! (learned hand-porting Mekanism — sawing/injecting/combining/cooking/
//! mek_data crafts) and rewrites them into the Horizon custom-recipe format
//! the engine's machine hooks read:
//!   {"type": "<out_ns>:<kind>", "input": <ingredient>, "result": {...}}
//!
//! Shape-based, not mod-specific: any mod whose recipes use the
//! item_input/main_input/extra_input/main_output field families converts
//! the same way. What the shapes mean (all verified against Mekanism 26.3
//! datagen output):
//!   item_input.ingredient + output           — item(→chemical) machine
//!   input.ingredient + main_output           — sawing-style (two outputs in
//!                                              Java; the secondary drops)
//!   main_input + extra_input + output        — dual-input (combiner-style)
//!   ingredient + result                      — vanilla cooking: carried
//!   pattern + key + result (modded type)     — mek_data-style shaped craft
//!   fluid_input/chemical_input without item  — fluid/gas-only: skipped
//! Vanilla `minecraft:*` types run as data in the engine already and carry.

use serde_json::Value;

/// What the converter decided for one recipe file.
pub enum Outcome {
    /// Keep the file as-is (vanilla type or unrecognized shape — the custom
    /// table tolerates unknown JSON, it just never matches).
    Carried,
    /// Rewrite with the converted JSON.
    Converted(Value),
    /// Drop the file: it can't run in an item-based machine (fluid/gas-only).
    /// Carries the reason for the report.
    Skipped(String),
}

/// Extracts an ingredient reference from every datagen generation:
/// - 26.x nested:  `{"count": 1, "ingredient": <ref>}` (ref = string/#tag/list)
/// - 1.21 flat:    `{"count": 1, "item": "x"}` / `{"count": 1, "tag": "x"}`
/// - already flat: a direct string, `#tag` or list
/// Chemical stacks (`{"amount": 1, "chemical": ...}`) extract their ref too.
fn ingredient(v: &Value) -> Option<Value> {
    match v {
        Value::String(_) | Value::Array(_) => Some(v.clone()),
        // neoforge:or / forge:or — any-of; a plain ingredient array means
        // the same thing to the engine's parser.
        Value::Object(o) if o.get("type").and_then(Value::as_str).is_some_and(|t| t.ends_with(":or")) => {
            ingredient(&Value::Array(o.get("values")?.as_array()?.clone()))
        }
        Value::Object(o) => {
            if let Some(g) = o.get("ingredient") {
                return ingredient(g);
            }
            if let Some(i) = o.get("item").and_then(Value::as_str) {
                return Some(Value::String(i.to_string()));
            }
            if let Some(t) = o.get("tag").and_then(Value::as_str) {
                return Some(Value::String(format!("#{t}")));
            }
            if let Some(c) = o.get("chemical").and_then(Value::as_str) {
                return Some(Value::String(c.to_string()));
            }
            None
        }
        _ => None,
    }
}

/// True when the recipe is gated on something the port can't satisfy —
/// another mod being loaded, or an item/tag from a foreign namespace.
/// Conditions on the mod's own items (neoforge:item_exists) and negations
/// (neoforge:not) keep the recipe: the porter can't fully evaluate them,
/// and a carried recipe is harmless where a skipped one is lost.
fn conditioned_on_other_mod(obj: &serde_json::Map<String, Value>, jar_ns: &str) -> Option<String> {
    /// The namespace of an id or "#tag" reference.
    fn ns_of(s: &str) -> &str {
        s.trim_start_matches('#').split(':').next().unwrap_or("")
    }
    for (k, v) in obj {
        if !k.ends_with(":conditions") {
            continue;
        }
        let Some(list) = v.as_array() else {
            continue;
        };
        for c in list {
            let ctype = c.get("type").and_then(Value::as_str).unwrap_or("");
            if ctype.ends_with("mod_loaded") {
                let modid = c.get("modid").and_then(Value::as_str).unwrap_or("");
                if !modid.is_empty() && modid != jar_ns && modid != "minecraft" {
                    return Some(format!("conditioned on {modid}"));
                }
            } else if ctype.ends_with("item_exists") || ctype.ends_with("tag_exists") {
                let id = c
                    .get("item")
                    .or_else(|| c.get("tag"))
                    .or_else(|| c.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let ns = ns_of(id);
                if !ns.is_empty() && ns != jar_ns && ns != "minecraft" && ns != "c" {
                    return Some(format!("conditioned on {ns}"));
                }
            }
            // neoforge:not and unknown condition types: keep the recipe.
        }
    }
    None
}

/// Converts one recipe's JSON. `jar_ns` is the namespace the jar's recipes
/// reference (`mekanism`), `out_ns` the ported mod's id (`mekanism_materials`).
pub fn convert(recipe: &Value, jar_ns: &str, out_ns: &str) -> Outcome {
    let Some(obj) = recipe.as_object() else {
        return Outcome::Carried;
    };
    let rtype = obj.get("type").and_then(Value::as_str).unwrap_or("");
    if rtype.starts_with("minecraft:") {
        return Outcome::Carried; // the engine's own serializers run these
    }
    if let Some(why) = conditioned_on_other_mod(obj, jar_ns) {
        return Outcome::Skipped(why);
    }

    // mek_data-style shaped crafts: a modded type with pattern+key+result is
    // a crafting recipe in disguise (Mekanism's tier factories).
    if obj.contains_key("pattern")
        && obj.contains_key("key")
        && obj.contains_key("result")
        && !rtype.is_empty()
    {
        let mut out = serde_json::Map::new();
        out.insert("type".into(), Value::String("minecraft:crafting_shaped".into()));
        if let Some(g) = obj.get("group") {
            out.insert("group".into(), g.clone());
        }
        out.insert("pattern".into(), obj["pattern"].clone());
        out.insert("key".into(), rename_ns(&obj["key"], jar_ns, out_ns));
        out.insert("result".into(), rename_ns(&obj["result"], jar_ns, out_ns));
        return Outcome::Converted(Value::Object(out));
    }

    // Fluid/gas-only recipes (rotary/chemical-infuser style): no item input.
    let item_input = ["item_input", "main_input", "input"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(ingredient));
    let chemical_only = (obj.contains_key("fluid_input")
        || obj.contains_key("chemical_input")
        || obj.contains_key("left_input")
        || obj.contains_key("right_input"))
        && item_input.is_none();
    if chemical_only {
        return Outcome::Skipped("fluid/gas-only (no item input)".into());
    }

    // The ingredient field families, by shape.
    let Some(ing) = item_input else {
        return Outcome::Carried;
    };
    let output = obj
        .get("output")
        .or_else(|| obj.get("main_output"))
        .or_else(|| obj.get("result"))
        .cloned();
    let Some(output) = output else {
        return Outcome::Carried;
    };
    let Some(out_id) = output.get("id").and_then(Value::as_str) else {
        return Outcome::Carried;
    };
    if out_id.is_empty() {
        return Outcome::Carried;
    }

    let kind = rtype.rsplit(':').next().unwrap_or(rtype);
    let mut out = serde_json::Map::new();
    out.insert(
        "type".into(),
        Value::String(format!("{out_ns}:{kind}")),
    );
    out.insert("input".into(), rename_ns(&ing, jar_ns, out_ns));
    // count from the output; a chemical output carries its amount instead
    // (the native side's "result names no registered item" rule reads it).
    let count = output
        .get("count")
        .and_then(Value::as_i64)
        .or_else(|| output.get("amount").and_then(Value::as_i64))
        .unwrap_or(1);
    let mut result = serde_json::Map::new();
    result.insert("id".into(), rename_ns(&Value::String(out_id.to_string()), jar_ns, out_ns));
    result.insert("count".into(), Value::from(count));
    out.insert("result".into(), Value::Object(result));
    // Dual-input and chemical fields ride along for native hooks that use
    // them (v0.5.0's extra_input; the future gas consumers' chemical_input).
    for field in ["extra_input", "chemical_input"] {
        if let Some(v) = obj.get(field).and_then(|f| ingredient(f)) {
            out.insert(field.into(), rename_ns(&v, jar_ns, out_ns));
        }
    }
    Outcome::Converted(Value::Object(out))
}

/// Rewrites `<jar_ns>:` (and `#<jar_ns>:`) references to `<out_ns>:` through
/// a whole JSON value, leaving other namespaces untouched.
fn rename_ns(v: &Value, jar_ns: &str, out_ns: &str) -> Value {
    match v {
        Value::String(s) => Value::String(rename_str(s, jar_ns, out_ns)),
        Value::Array(a) => Value::Array(a.iter().map(|x| rename_ns(x, jar_ns, out_ns)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), rename_ns(x, jar_ns, out_ns)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn rename_str(s: &str, jar_ns: &str, out_ns: &str) -> String {
    let body = s.strip_prefix('#').unwrap_or(s);
    if let Some(rest) = body.strip_prefix(&format!("{jar_ns}:")) {
        format!("{}{}:{rest}", if s.starts_with('#') { "#" } else { "" }, out_ns)
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sawing_shape_reads_main_output() {
        let r = json!({
            "type": "mekanism:sawing", "group": "planks",
            "input": {"count": 1, "ingredient": "minecraft:barrel"},
            "main_output": {"count": 7, "id": "minecraft:oak_planks"},
            "secondary_output": {"id": "mekanism:sawdust"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["type"], "mm:sawing");
                assert_eq!(v["input"], "minecraft:barrel");
                assert_eq!(v["result"]["id"], "minecraft:oak_planks");
                assert_eq!(v["result"]["count"], 7);
                assert!(v.get("secondary_output").is_none());
            }
            _ => panic!("sawing should convert"),
        }
    }

    #[test]
    fn injecting_shape_keeps_chemical_and_extra() {
        let r = json!({
            "type": "mekanism:injecting",
            "item_input": {"count": 1, "ingredient": "#c:bricks/normal"},
            "chemical_input": {"amount": 1, "ingredient": "mekanism:water_vapor"},
            "output": {"id": "minecraft:clay_ball"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["input"], "#c:bricks/normal");
                assert_eq!(v["chemical_input"], "mm:water_vapor");
                assert_eq!(v["result"]["id"], "minecraft:clay_ball");
            }
            _ => panic!("injecting should convert"),
        }
    }

    #[test]
    fn combining_shape_reads_extra_input() {
        let r = json!({
            "type": "mekanism:combining",
            "main_input": {"count": 1, "ingredient": "#c:dyes/blue"},
            "extra_input": {"count": 1, "ingredient": "#c:dyes/green"},
            "output": {"count": 4, "id": "minecraft:cyan_dye"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["input"], "#c:dyes/blue");
                assert_eq!(v["extra_input"], "#c:dyes/green");
                assert_eq!(v["result"]["count"], 4);
            }
            _ => panic!("combining should convert"),
        }
    }

    #[test]
    fn mek_data_becomes_crafting_shaped() {
        let r = json!({
            "type": "mekanism:mek_data", "group": "mekanism:crushing",
            "key": {"A": "#mekanism:alloys/basic", "P": "mekanism:crusher"},
            "pattern": ["ACA", "IPI", "ACA"],
            "result": {"id": "mekanism:basic_crushing_factory"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["type"], "minecraft:crafting_shaped");
                assert_eq!(v["key"]["A"], "#mm:alloys/basic");
                assert_eq!(v["key"]["P"], "mm:crusher");
                assert_eq!(v["result"]["id"], "mm:basic_crushing_factory");
            }
            _ => panic!("mek_data should convert"),
        }
    }

    #[test]
    fn gas_output_carries_amount_as_count() {
        let r = json!({
            "type": "mekanism:oxidizing",
            "input": {"count": 1, "ingredient": "#c:fuels/bio"},
            "output": {"amount": 5, "id": "mekanism:bio"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["result"]["id"], "mm:bio");
                assert_eq!(v["result"]["count"], 5);
            }
            _ => panic!("gas output should convert"),
        }
    }

    #[test]
    fn rotary_is_skipped() {
        let r = json!({
            "type": "mekanism:rotary",
            "fluid_input": {"amount": 1, "ingredient": "#c:brine"},
            "chemical_output": {"amount": 1, "id": "mekanism:brine"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Skipped(why) => assert!(why.contains("fluid/gas")),
            _ => panic!("rotary should be skipped"),
        }
    }

    #[test]
    fn vanilla_smelting_carries() {
        let r = json!({
            "type": "minecraft:smelting", "cookingtime": 200,
            "ingredient": "mekanism:dust_lead",
            "result": {"id": "mekanism:ingot_lead"}
        });
        assert!(matches!(convert(&r, "mekanism", "mm"), Outcome::Carried));
    }

    #[test]
    fn namespace_rename_hits_tags_too() {
        assert_eq!(rename_str("#mekanism:alloys/basic", "mekanism", "mm"), "#mm:alloys/basic");
        assert_eq!(rename_str("mekanism:crusher", "mekanism", "mm"), "mm:crusher");
        assert_eq!(rename_str("minecraft:barrel", "mekanism", "mm"), "minecraft:barrel");
        assert_eq!(rename_str("mekanismtools:bronze_axe", "mekanism", "mm"), "mekanismtools:bronze_axe");
    }
}

#[cfg(test)]
mod flat_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_121_item_input() {
        let r = json!({
            "type": "mekanism:crushing",
            "input": {"count": 1, "item": "minecraft:flint"},
            "output": {"count": 1, "id": "mekanism:dust_flint"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["input"], "minecraft:flint");
                assert_eq!(v["result"]["id"], "mm:dust_flint");
            }
            _ => panic!("flat item input should convert"),
        }
    }

    #[test]
    fn flat_121_tag_input() {
        let r = json!({
            "type": "mekanism:combining",
            "main_input": {"count": 1, "item": "minecraft:flint"},
            "extra_input": {"count": 1, "tag": "c:cobblestones/normal"},
            "output": {"count": 1, "id": "minecraft:gravel"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["input"], "minecraft:flint");
                assert_eq!(v["extra_input"], "#c:cobblestones/normal");
            }
            _ => panic!("flat tag input should convert"),
        }
    }

    #[test]
    fn conditioned_on_other_mod_skips() {
        let r = json!({
            "type": "mekanism:crushing",
            "neoforge:conditions": [{"type": "neoforge:mod_loaded", "modid": "ae2"}],
            "input": {"count": 1, "tag": "c:gems/certus_quartz"},
            "output": {"count": 1, "id": "ae2:certus_quartz_dust"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Skipped(why) => assert!(why.contains("ae2")),
            _ => panic!("foreign-mod condition should skip"),
        }
    }

    #[test]
    fn chemical_infuser_skips() {
        let r = json!({
            "type": "mekanism:chemical_infusing",
            "left_input": {"amount": 1, "chemical": "mekanism:hydrogen"},
            "right_input": {"amount": 1, "chemical": "mekanism:chlorine"},
            "output": {"amount": 1, "id": "mekanism:hydrogen_chloride"}
        });
        assert!(matches!(convert(&r, "mekanism", "mm"), Outcome::Skipped(_)));
    }

    #[test]
    fn nested_26x_still_converts() {
        let r = json!({
            "type": "mekanism:sawing",
            "input": {"count": 1, "ingredient": "#minecraft:oak_logs"},
            "main_output": {"count": 6, "id": "minecraft:oak_planks"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => assert_eq!(v["input"], "#minecraft:oak_logs"),
            _ => panic!("26.x nested shape must keep converting"),
        }
    }
}

#[cfg(test)]
mod or_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn neoforge_or_unwraps_to_array() {
        let r = json!({
            "type": "mekanism:crushing",
            "input": {"type": "neoforge:or", "values": ["#c:crops/wheat", "#c:crops/carrot"]},
            "output": {"count": 2, "id": "mekanism:bio_fuel"}
        });
        match convert(&r, "mekanism", "mm") {
            Outcome::Converted(v) => {
                assert_eq!(v["input"], json!(["#c:crops/wheat", "#c:crops/carrot"]));
            }
            _ => panic!("or-ingredient should unwrap"),
        }
    }
}
