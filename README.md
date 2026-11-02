# hmod-porter

**Turn a Java Minecraft mod jar into a Horizon content mod (`.hmod`) — automatically.**

Horizon is Minecraft: Java Edition 26.3 rewritten in Rust. It runs no Java, so Java mods
cannot load. `hmod-porter` reads a mod's jar and emits a Horizon content mod: blocks, items,
recipes, tags, models and textures, with the declarations Horizon needs.

```sh
hmod-porter Mekanism-1.21.1.jar out/
cd out && cargo horizon-mod build
```

## What it does

| Stage | What happens |
|---|---|
| **Read** | Unpacks the jar, reads `META-INF/neoforge.mods.toml` / `fabric.mod.json` for the mod id |
| **Classify** | Decides what carries (assets, data, recipes, tags) and what cannot (Java classes, worldgen, companion-library namespaces) |
| **Convert recipes** | Rewrites modded recipe shapes into Horizon's custom-recipe format |
| **Declare** | Emits `data/<ns>/horizon/block/*.json` and `item/*.json` — each block/item declared on a vanilla template |
| **Fit** | Reconciles the carried content with the declarations (see below) |
| **Write** | Produces a buildable `.hmod` source tree |

## The fit layer

Every rule in `src/fit.rs` came from a bug found while porting Mekanism in game. They are
generic — any mod hits them:

1. **Composite flattening.** A `neoforge:composite` model is a NeoForge loader feature.
   Horizon has no loaders: it reads the model, finds no `elements` and no usable `parent`,
   and draws **nothing** — the block is invisible and the floor shows through. The porter
   merges the children into one flat model. *(Mekanism: 66 models — every factory.)*
2. **Template/blockstate property match.** A block declared on the property-less
   `minecraft:iron_block` template whose blockstate keys on `facing`/`lit` renders as the
   missing-texture checkerboard. The porter collapses it to one property-less variant.
   *(Mekanism: 50 blocks.)*
3. **Orphan parents.** A model naming a parent that was not carried renders invisible or
   checkered; the porter reports them and repoints `*/base` orphans to the module base.
4. **Missing textures.** Every `ns:block/<path>` a model names must have a PNG; the porter
   reports the model and the texture so you know which faces will checkerboard.

The fit pass prints one line per port:

```
fit: 74 composite(s) flattened, 0 blockstate(s) collapsed, 0 orphan parent(s), 0 missing texture(s)
```

## Recipe conversion

Modded recipes are converted by **shape**, not by mod name (`src/recipes.rs`). Both datagen
generations are handled:

- 26.x nested: `{"input": {"count": 1, "ingredient": "#c:ingots/iron"}}`
- 1.21 flat: `{"input": {"count": 1, "item": "minecraft:flint"}}` / `{"tag": "..."}`

Recognised shapes: `item_input`/`main_input`/`input` + `output`/`main_output`/`result`,
`extra_input` (dual-input machines), `chemical_input` (gas consumers), `neoforge:or`
(any-of, unwrapped to an array), `mek_data`-style shaped crafts (rewritten to
`minecraft:crafting_shaped`), and chemical outputs (`amount` becomes `count`).

Recipes gated on another mod (`neoforge:mod_loaded`, `item_exists` on a foreign namespace)
are skipped with a reason; conditions on the mod's own items are kept.

**Measured on the real Mekanism jar: 1076 recipes converted, 728 skipped (fluid/gas-only or
foreign-mod-gated).**

## What it cannot do

- **Java code.** Machines, GUIs and systems are Java classes; a content mod is data only.
  Porting behaviour needs a native mod (`horizon-mixins`) — that is a separate, manual job.
- **Worldgen.** `worldgen/`, `neoforge/biome_modifier` are not part of Horizon's content
  capability yet; those files are reported as blocked.
- **Companion-library namespaces** (curios & co.) are never declared — two ports of mods
  that both use them would collide.

## Usage

```sh
# basic
hmod-porter <mod.jar> [out-dir]

# with an oracle facts file (exact block/item properties instead of name heuristics)
hmod-porter --facts facts.json <mod.jar> [out-dir]
```

The output is a Horizon mod source tree: `horizon-mod.toml`, `assets/`, `data/`. Build it
with `cargo horizon-mod build` (Horizon's own toolchain).

## Status

Early but working. Verified against six real mods (Mekanism, Ender IO, Industrial Foregoing,
Refined Storage, Traveler's Backpack, Useful Backpacks): all port without errors, and the
Mekanism port is playable in game (199 blocks, 219 items, 1076 recipes, all names, all
models rendering).

30 unit tests cover the recipe shapes, the fit rules and the template heuristics.

## Licence

MIT. Ported mod content keeps its own licence — the porter only reads the jar you give it
and never ships mod assets itself.
