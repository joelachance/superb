//! Scans the default CLAP directories (plus `CLAP_PATH` and any directory
//! given on the command line), prints every plugin found, then instantiates
//! the first audio effect (or the plugin whose ID matches `--id <id>`), lists
//! its parameters and renders one second of a sine through it offline.
//!
//! What you'll learn:
//! - Parsing simple CLI args with `std::env::args`
//! - Scanning plugin bundles and picking one with iterators
//! - Wrapping a `PluginInstance` in `PluginNode` (`Box<dyn Node>`)
//! - Offline `Engine` render through a graph (no audio hardware)
//!
//! ```text
//! cargo run -p clap_host_scan -- [dir ...] [--id com.vendor.plugin]
//! ```
//!
//! Without any CLAP plugin installed the example still runs and reports an
//! empty scan. No audio hardware is used.

use std::path::PathBuf;

use superb::core::{Result, SampleRate};
use superb::graph::nodes::SineNode;
use superb::graph::AudioGraph;
use superb::io::{offline, Engine};
use superb::plugins::clap::ClapBundle;
use superb::plugins::scan::{default_search_paths, scan_default, scan_directory, ScanReport};
use superb::plugins::{PluginFormat, PluginInstance, PluginNode};
use superb::transport::Transport;

// Result<()> means "succeed with nothing, or return an error".
// The `?` operator below forwards errors up to main's caller (the OS).
fn main() -> Result<()> {
    // Collect optional search dirs and an optional --id filter from argv.
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut wanted_id: Option<String> = None;
    // skip(1) drops the program name; while let Some(a) walks remaining args.
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--id" {
            // --id consumes the next token as the plugin ID string.
            wanted_id = args.next();
        } else {
            // Anything else is treated as an extra directory to scan.
            dirs.push(PathBuf::from(a));
        }
    }

    println!("default CLAP search paths:");
    for p in default_search_paths(PluginFormat::Clap) {
        // '+' means the path exists as a directory on this machine.
        println!("  {} {}", if p.is_dir() { "+" } else { "-" }, p.display());
    }

    // Scan OS defaults (+ CLAP_PATH). `?` returns early if the scan itself errors.
    let mut report: ScanReport = scan_default(PluginFormat::Clap)?;
    for dir in &dirs {
        // &dirs borrows each PathBuf; match handles per-directory failures softly.
        match scan_directory(dir, PluginFormat::Clap) {
            Ok(part) => {
                report.plugins.extend(part.plugins);
                report.failures.extend(part.failures);
            }
            Err(e) => println!("skipping {}: {e}", dir.display()),
        }
    }
    println!(
        "\nfound {} plugin(s), {} bundle(s) failed to load",
        report.plugins.len(),
        report.failures.len()
    );
    for d in &report.plugins {
        println!(
            "  {:<40} {:<24} v{:<8} [{}]  {}",
            d.id,
            d.name,
            d.version,
            d.features.join(", "),
            d.path.display()
        );
    }
    for (path, why) in &report.failures {
        println!("  ! {}: {why}", path.display());
    }

    // Prefer --id if given; otherwise the first descriptor marked as an audio effect.
    let chosen = match &wanted_id {
        Some(id) => report.plugins.iter().find(|d| &d.id == id),
        None => report.plugins.iter().find(|d| d.is_audio_effect()),
    };
    // let Some(desc) = ... else { ... } is Rust's concise "early return if None".
    let Some(desc) = chosen else {
        println!("\nnothing to instantiate (no audio effect found); pass a directory or --id to pick one");
        return Ok(());
    };

    println!("\ninstantiating {} ({})", desc.name, desc.id);
    // Load the .clap bundle from disk, then create one plugin instance by ID.
    let bundle = ClapBundle::load(&desc.path)?;
    let plugin = bundle.instantiate(&desc.id)?;
    println!("ports: {:?}", plugin.ports());
    println!("{} parameter(s):", plugin.params().len());
    for p in plugin.params() {
        // unwrap_or: if the param has no current value, print NaN instead of panicking.
        let value = plugin.param_value(p.id).unwrap_or(f64::NAN);
        // Build a short flag string from the param's boolean attributes.
        let flags = [
            (p.automatable, "auto"),
            (p.stepped, "stepped"),
            (p.hidden, "hidden"),
            (p.readonly, "ro"),
            (p.bypass, "bypass"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(",");
        println!(
            "  #{:<6} {:<32} {:>10.4} in [{}, {}] default {} {}",
            p.id, p.name, value, p.min, p.max, p.default, flags
        );
    }

    // Offline graph: sine → plugin → output, then render 1 second at 48 kHz.
    let rate = SampleRate::HZ_48K;
    let mut graph = AudioGraph::new();
    // Box::new(...) heap-allocates a concrete node so the graph can store dyn Node.
    let osc = graph.add_node(Box::new(SineNode::new(
        440.0,
        0.5,
        plugin.ports().inputs.max(1),
    )));
    // PluginNode adapts PluginInstance to the graph's Node trait (another Box).
    let fx = graph.add_node(Box::new(PluginNode::new(Box::new(plugin))));
    if graph.connect(osc, fx).is_err() {
        println!("plugin has no audio input (instrument?) — rendering it unconnected");
    }
    graph.set_output(fx)?;
    let mut engine = Engine::new(graph, Transport::new(rate, 120.0));
    engine.transport_mut().play();
    // render(engine, rate, block_size, frame_count) → PlanarBuffer of audio.
    let audio = offline::render(&mut engine, rate, 256, rate.hz() as usize)?;
    let latency = engine.processor().latency_frames();
    println!(
        "\nrendered {} frames x {} ch through the plugin: peak {:.4}, reported latency {} frames",
        audio.frames(),
        audio.channels(),
        audio.peak(),
        latency
    );
    Ok(())
}
