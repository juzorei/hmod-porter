//! hmod-porter: ports the data layer of Java Minecraft mods to Horizon
//! content mods (.hmod). Carries vanilla-format data and assets, derives
//! block/item declarations (oracle facts first, name-shape heuristics as
//! fallback), and reports everything that needs engine capabilities
//! Horizon's modding API doesn't have yet — as an honest boundary report,
//! never silently.

mod classify;
mod emit;
mod facts;
mod fit;
mod jar;
mod recipes;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use classify::Kind;

const USAGE: &str = "usage: hmod-porter [--facts <oracle-facts.json>] <mod.jar> [out-dir]
  --facts  NeoForge oracle export (tools/neoforge_oracle.py export):
           exact hardness/components replace name-shape heuristics.
  out-dir  default: ./<mod-id>";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{USAGE}");
        std::process::exit(if args.is_empty() { 2 } else { 0 });
    }

    let mut facts_path: Option<PathBuf> = None;
    let mut positional = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--facts" => {
                facts_path = Some(PathBuf::from(it.next().context("--facts needs a path")?));
            }
            _ => positional.push(a),
        }
    }
    if positional.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let jar_path = PathBuf::from(&positional[0]);
    let out_dir = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(emit::sanitize_id(jar_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ported"))));

    let exit = run(&jar_path, &out_dir, facts_path.as_deref())?;
    std::process::exit(exit);
}

fn run(jar_path: &Path, out_dir: &Path, facts_path: Option<&Path>) -> Result<i32> {
    println!("reading {}", jar_path.display());
    let (contents, manifest) = jar::read_jar(jar_path)
        .with_context(|| format!("reading {}", jar_path.display()))?;
    println!(
        "mod: {} v{} [{}] ({} classes, {} content files, {:.1} KiB content)",
        manifest.mod_id,
        manifest.version.as_deref().unwrap_or("?"),
        manifest.loader,
        contents.classes,
        contents.content.len(),
        contents.total_content_bytes() as f64 / 1024.0,
    );

    let oracle = match facts_path {
        Some(p) => {
            println!("facts: {} (exact properties where available)", p.display());
            facts::load(p)?
        }
        None => {
            println!("facts: none — name-shape heuristics; pass --facts <oracle facts.json> for exact properties");
            facts::Facts::default()
        }
    };

    let report = classify::classify(&contents, &manifest.mod_id);
    let v = classify::verdict(&manifest.mod_id, &report);

    let outcome = emit::write_hmod(out_dir, &manifest, &report, &oracle, &|name| {
        contents.content.get(name).map(|e| match e {
            jar::Entry::Resource(b) => b.clone(),
            _ => Vec::new(),
        })
    })?;

    // The fit layer: reconcile the carried content with the declarations
    // (template/blockstate properties, orphan model parents, missing
    // textures). Every rule here came from a bug the Mekanism port hit in
    // game — see src/fit.rs.
    let mod_id = emit::sanitize_id(&manifest.mod_id);
    match fit::run(out_dir, &mod_id) {
        Ok(fit_report) => {
            if !fit_report.is_clean() {
                let repaired = fit::repair_orphan_parents(out_dir, &mod_id, &fit_report.orphan_parents)
                    .unwrap_or(0);
                println!(
                    "{} ({} orphan parent(s) repointed to the module base)",
                    fit_report.summary(),
                    repaired
                );
                if !fit_report.missing_textures.is_empty() {
                    println!("  missing textures (checkerboard faces):");
                    for (model, tex) in fit_report.missing_textures.iter().take(10) {
                        println!("    {model} -> {tex}");
                    }
                    if fit_report.missing_textures.len() > 10 {
                        println!("    ... and {} more", fit_report.missing_textures.len() - 10);
                    }
                }
            } else {
                println!("{}", fit_report.summary());
            }
        }
        Err(e) => println!("fit: could not run ({e}) — the pack is still written"),
    }
    print_report(&v.kind, &report, &outcome);
    println!("wrote .hmod source to {}", out_dir.display());
    println!(
        "next: cd {} && cargo horizon-mod build && cargo horizon-mod check target/horizon-mod/<id>.hmod",
        out_dir.display()
    );
    Ok(match v.kind {
        Kind::ContentOnly => 0,
        Kind::Partial { .. } => 0,
        Kind::Blocked { .. } => 1,
    })
}

fn print_report(kind: &Kind, report: &classify::Report, outcome: &emit::GenOutcome) {
    println!("\n=== verdict ===");
    match kind {
        Kind::ContentOnly => println!("CONTENT (fully portable today)"),
        Kind::Partial { carried } => println!("PARTIAL ({carried} files carried)"),
        Kind::Blocked { by } => println!("BLOCKED (needs {})", by.roadmap_ref()),
    }
    println!(
        "carried: {} data files, {} asset files | declared {} blocks ({} exact), {} items ({} exact) | {} loot tables generated",
        report.carry_files.len(),
        report.carry_assets.len(),
        outcome.declared_blocks,
        outcome.exact_blocks,
        outcome.declared_items,
        outcome.exact_items,
        outcome.generated_loot,
    );
    if !report.blocked.is_empty() {
        println!("blocked parts:");
        let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
        for by in report.blocked.values() {
            *by_reason.entry(by.roadmap_ref().to_string()).or_default() += 1;
        }
        for (reason, n) in by_reason {
            println!("  {n} file(s) need {reason}");
        }
    }
    for s in &outcome.skipped {
        println!("  skipped (no template heuristic): {s}");
    }
    println!();
}
