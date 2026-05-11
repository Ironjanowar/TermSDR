use anyhow::Result;

fn main() -> Result<()> {
    println!("TermSDR v0.1.0");
    println!("Terminal-based SDR application");
    println!();
    println!("TODO: Wire up CLI args -> config -> source -> DSP -> UI pipeline");
    println!("Run with --test to use synthetic test signals.");

    // TODO: Parse CLI args, create source, start DSP pipeline, launch TUI
    Ok(())
}
