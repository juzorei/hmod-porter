//! Classifies a jar's content: what a Horizon content mod (API 0.2) can
//! carry today, and what is blocked by which missing engine capability.
//! The report is the porter's honest boundary — no silent leftovers.

use std::collections::BTreeMap;

use crate::jar::{Entry, JarContents};

/// Subsystem of the engine a blocked item waits for (matches Horizon's
/// roadmap item names, so the report doubles as a feature request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockedBy {
    /// Ticking block entities — machines, pipes (Horizon "machines with
    /// inventories and menus", #1189).
    Machines,
    /// Items that carry runtime data / are containers (backpacks etc.).
    ItemData,
    /// Custom entities/mobs behaviour.
    Entities,
    /// World generation: placed features, biomes (datapack worldgen).
    Worldgen,
    /// Custom networking the mod's client code needs.
    Networking,
    /// Behaviour bound to a vanilla item id (a copied trident that throws,
    /// Horizon #3305) — engine-side, not data.
    BoundBehaviour,
    /// Files outside assets/<ns>/... / data/<ns>/... — jar strays Horizon
    /// refuses to pack (e.g. Industrial Foregoing's cat.jpg).
    NotNamespaced,
    /// Data under companion-library namespaces (curios, trinkets, titanium...)
    /// whose code never ports; carried, they clash between mods.
    ForeignNamespace,
}

impl BlockedBy {
    pub fn roadmap_ref(self) -> &'static str {
        match self {
            BlockedBy::Machines => "machines with inventories and menus (#1189)",
            BlockedBy::ItemData => "item data / containers",
            BlockedBy::Entities => "entities & mob behaviour",
            BlockedBy::Worldgen => "modded worldgen in content mods",
            BlockedBy::Networking => "mod networking",
            BlockedBy::BoundBehaviour => "id-bound behaviour (#3305)",
            BlockedBy::NotNamespaced => "namespaced path (jar strays)",
            BlockedBy::ForeignNamespace => "companion-library namespace (curios etc.)",
        }
    }
}

#[derive(Debug)]
pub struct Verdict {
    pub id: String,
    pub kind: Kind,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    /// Fully data-driven: assets + data only, no code.
    ContentOnly,
    /// Data present but code exists too: carried parts ship, rest is listed.
    Partial { carried: usize },
    /// Code-only: nothing a content mod can run today.
    Blocked { by: BlockedBy },
}

#[derive(Debug, Default)]
pub struct Report {
    /// Data/<ns>/... files a .hmod can take unchanged.
    pub carry_files: BTreeMap<String, Entry>,
    /// Assets/<ns>/... files a .hmod can take unchanged.
    pub carry_assets: BTreeMap<String, Entry>,
    /// Entries dropped because they use mod machinery.
    pub blocked: BTreeMap<String, BlockedBy>,
    pub classes: usize,
}

impl Report {
    pub fn carried_count(&self) -> usize {
        self.carry_files.len() + self.carry_assets.len()
    }
}

/// A recipe file whose "type" is not a vanilla/known Horizon type: it needs
/// the mod's code and only pollutes the game log if carried.
#[derive(Debug, Clone)]
pub struct NonVanilla {
    pub recipe_type: String,
}

/// Recipe serializers a data-only mod can actually run: the vanilla 26.3
/// set Horizon's loader implements. Types outside it need the mod's code.
/// Blockstate properties a data-only mod can express: the vanilla set.
/// Blockstates carrying anything else (mods' own properties — an inventory
/// direction, energy type...) describe machine behaviour and crash the
/// loader ("invalid climbing facing" class of errors).
pub const VANILLA_STATE_PROPERTIES: &[&str] = &[
    "facing", "face", "axis", "half", "part", "powered", "active", "lit",
    "open", "occupied", "type", "waterlogged", "shape", "rotation",
    "snowiness", "bottom", "enabled", "drag", "in_wall", "hinge", "door_hinge",
    "has_record", "has_bottle_0", "has_bottle_1", "has_bottle_2",
    "layers", "level", "bites", "honey_level", "stage", "age", "growth",
    "charge", "attached", "triggered", "unstable", "disarmed", "burst",
    "explode_bit", "drag_down", "dusted", "eggs", "hatch", "books",
    "attachment", "cracked", "cardinal_direction",
    "orientation", "vault_state", "ominous", "slot_0_occupied",
    "south", "north", "east", "west", "up", "down", "top_level",
];

/// Blockstate files whose variant keys use non-vanilla properties ("direction="
/// with diagonal values, "energy_type", custom enums): machine-driven, drop.
fn blockstate_is_vanilla(v: &serde_json::Value) -> bool {
    if v.get("multipart").is_some() {
        return false;
    }
    let Some(variants) = v.get("variants").and_then(|x| x.as_object()) else {
        return true; // empty/model-less — harmless
    };
    for key in variants.keys() {
        if key == "" {
            continue; // the no-state block: fine
        }
        for part in key.split(',') {
            let Some((name, _value)) = part.split_once('=') else { continue };
            if !VANILLA_STATE_PROPERTIES.contains(&name) {
                return false;
            }
        }
    }
    true
}

const VANILLA_RECIPE_TYPES: &[&str] = &[
    "minecraft:crafting_shaped",
    "minecraft:crafting_shapeless",
    "minecraft:crafting_transmute",
    "minecraft:crafting_special_armordye",
    "minecraft:crafting_special_bannerduplicate",
    "minecraft:crafting_special_bookcloning",
    "minecraft:crafting_special_firework_rocket",
    "minecraft:crafting_special_firework_star",
    "minecraft:crafting_special_firework_starfade",
    "minecraft:crafting_special_mapcloning",
    "minecraft:crafting_special_mapextending",
    "minecraft:crafting_special_repairitem",
    "minecraft:crafting_special_shielddecoration",
    "minecraft:crafting_special_shulkerboxcoloring",
    "minecraft:crafting_special_suspiciousstew",
    "minecraft:crafting_special_tippedarrow",
    "minecraft:smelting",
    "minecraft:blasting",
    "minecraft:smoking",
    "minecraft:campfire_cooking",
    "minecraft:stonecutting",
    "minecraft:smithing_transform",
    "minecraft:smithing_trim",
];

pub fn classify(contents: &JarContents, mod_id: &str) -> Report {
    let own = mod_id.to_ascii_lowercase();
    let mut report = Report { classes: contents.classes, ..Report::default() };

    for (name, entry) in &contents.content {
        let mut blocked_by = classify_entry(name, entry);

        // Companion-library namespaces (curios, trinkets, titanium...) belong
        // to other mods' code, which never ports; their data files only
        // collide between ports. Only the mod's own, minecraft (tags) and
        // c (convention) survive.
        if blocked_by.is_none() {
            if let Some(ns) = name.split('/').nth(1) {
                if !matches!(ns, "minecraft" | "c" | "horizon") && ns != own {
                    blocked_by = Some(BlockedBy::ForeignNamespace);
                }
            }
        }
        match blocked_by {
            Some(by) => {
                report.blocked.insert(name.clone(), by);
            }
            None => {
                if is_asset(name) {
                    report.carry_assets.insert(name.clone(), entry.clone());
                } else {
                    report.carry_files.insert(name.clone(), entry.clone());
                }
            }
        }
    }

    report
}

fn is_asset(name: &str) -> bool {
    name.starts_with("assets/")
}

fn classify_entry(name: &str, _entry: &Entry) -> Option<BlockedBy> {
    let parts: Vec<&str> = name.splitn(3, '/').collect();
    if parts.len() < 3 {
        // Horizon packs only assets/<ns>/... and data/<ns>/...; stray root
        // files (e.g. IF's assets/cat.jpg) refuse the whole build.
        return Some(BlockedBy::NotNamespaced);
    }
    let (kind, _ns, rest) = (parts[0], parts[1], parts[2]);

    // Only lowercase, modern-pack resource files carry: legacy .lang files,
    // .obj/.mtl models and other loader-era formats are refused by the packer.
    if let Some(ext) = rest.rsplit('.').next() {
        if !["json", "png", "ogg", "mcmeta", "nbt"].contains(&ext) || rest.chars().any(|c| c.is_ascii_uppercase()) {
            return Some(BlockedBy::NotNamespaced);
        }
    }
    debug_assert!(kind == "data" || kind == "assets");

    if kind == "assets" {
        // Blockstates carry whole — multipart and mod-property variants
        // included: the loader logs what it cannot bind (as vanilla data
        // packs do) instead of the porter dropping the block's looks.
        if rest.starts_with("blockstates/") {
            return None;
        }
    }

    if kind == "data" {
        // Recipes carry whole: vanilla serializers run as data, custom types
        // ("mekanism:enriching"...) land in the engine's custom-recipe table
        // where machine mods (native hooks) read them by type.
        if rest.starts_with("recipe/") {
            return None;
        }
        // Worldgen: placed features/biomes are not part of capability
        // "registries" yet.
        if rest.starts_with("worldgen/")
            || rest.starts_with("neoforge/biome_modifier")
            || rest.starts_with("forge/biome_modifier")
            || rest.starts_with("fabric/biome_modifier")
        {
            return Some(BlockedBy::Worldgen);
        }
    }
    None
}

/// Summarises the report into a single verdict line for the CLI and tests.
pub fn verdict(ns: &str, report: &Report) -> Verdict {
    if report.carried_count() == 0 {
        return Verdict { id: ns.into(), kind: Kind::Blocked { by: BlockedBy::Machines } };
    }
    if report.classes == 0 {
        return Verdict { id: ns.into(), kind: Kind::ContentOnly };
    }
    Verdict {
        id: ns.into(),
        kind: Kind::Partial { carried: report.carried_count() },
    }
}
