mod capture;
mod error;
mod layout;
mod selector;
mod split;

use crate::error::Result;
use clap::{Args, Parser, Subcommand};
use selector::{FileSelector, InteractiveSelector, SelectAll};

// Re-export for tests

#[derive(Parser)]
#[command(name = "c2rust-demo")]
#[command(about = "Minimal C-to-Rust workflow: build capture + Rust scaffolding")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Capture a C build and generate Rust scaffolding
    Init(InitArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Feature name (default: "default")
    #[arg(long, default_value = "default")]
    feature: String,

    /// Skip interactive file selection – include all captured files
    #[arg(long)]
    no_interactive: bool,

    /// Build command to execute (use after '--')
    /// Example: c2rust-demo init -- make -j4
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        required = true,
        value_name = "BUILD_CMD"
    )]
    build_cmd: Vec<String>,
}

fn run_init(args: InitArgs) -> Result<()> {
    let feature = &args.feature;
    let build_cmd = &args.build_cmd;

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("current_dir: {}", e))?;
    let project_root = layout::find_project_root(&cwd);

    println!("=== c2rust-demo init ===");
    println!("Project root : {}", project_root.display());
    println!("Feature      : {}", feature);
    println!("Build command: {}", build_cmd.join(" "));
    println!();

    // Create layout directories
    let lo = layout::FeatureLayout::new(project_root.clone(), feature);
    lo.create_dirs()?;

    // Save build command metadata
    lo.save_build_cmd(build_cmd)?;

    // ----------------------------------------------------------------
    // Step 1: build hook library
    // ----------------------------------------------------------------
    let hook_so = capture::build_hook()?;

    // ----------------------------------------------------------------
    // Step 2: run build with LD_PRELOAD
    // ----------------------------------------------------------------
    capture::run_with_hook(&cwd, build_cmd, &project_root, &lo.feature_root, &hook_so)?;

    // ----------------------------------------------------------------
    // Step 3: scan captured .c2rust files
    // ----------------------------------------------------------------
    let captured = layout::scan_c2rust_files(&lo.c_dir)?;
    println!("\nCaptured {} .c2rust file(s)", captured.len());

    if captured.is_empty() {
        println!("Warning: no .c2rust files were generated.");
        println!("Make sure your build command compiles C files.");
        return Ok(());
    }

    // ----------------------------------------------------------------
    // Step 4: interactive (or automatic) file selection
    // ----------------------------------------------------------------
    let sel: Box<dyn FileSelector> = if args.no_interactive {
        Box::new(SelectAll)
    } else {
        Box::new(InteractiveSelector)
    };
    let selected = sel.select(&captured)?;
    println!("{} file(s) selected for this feature", selected.len());

    lo.save_selected_files(&selected)?;

    if selected.is_empty() {
        println!("No files selected – skipping split.");
        return Ok(());
    }

    // ----------------------------------------------------------------
    // Step 5: init split (create Rust project + generate scaffolding)
    // ----------------------------------------------------------------
    println!("\nRunning init split...");
    let feature_obj = split::Feature::new_with_selection(&project_root, feature, &selected)?;
    feature_obj.init()?;

    println!("\n✓ c2rust-demo init completed successfully!");
    println!("\nOutput structure:");
    println!("  .c2rust/{}/", feature);
    println!("    ├── c/          (captured .c2rust files)");
    println!("    ├── meta/       (build_cmd.txt, selected_files.json)");
    println!("    └── rust/       (generated Rust project)");

    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Init(args) => run_init(args),
    };
    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_help_does_not_panic() {
        Cli::command().debug_assert();
    }

    #[test]
    fn init_default_feature() {
        let args = Cli::try_parse_from(["c2rust-demo", "init", "--", "make"]).unwrap();
        let Commands::Init(init) = args.command;
        assert_eq!(init.feature, "default");
        assert_eq!(init.build_cmd, vec!["make"]);
    }

    #[test]
    fn init_custom_feature() {
        let args =
            Cli::try_parse_from(["c2rust-demo", "init", "--feature", "myfeature", "--", "make"])
                .unwrap();
        let Commands::Init(init) = args.command;
        assert_eq!(init.feature, "myfeature");
    }

    #[test]
    fn init_no_interactive_flag() {
        let args = Cli::try_parse_from([
            "c2rust-demo",
            "init",
            "--no-interactive",
            "--",
            "make",
            "-j4",
        ])
        .unwrap();
        let Commands::Init(init) = args.command;
        assert!(init.no_interactive);
        assert_eq!(init.build_cmd, vec!["make", "-j4"]);
    }

    #[test]
    fn init_multi_word_build_cmd() {
        let args = Cli::try_parse_from([
            "c2rust-demo",
            "init",
            "--",
            "make",
            "CFLAGS=-O2",
            "all",
        ])
        .unwrap();
        let Commands::Init(init) = args.command;
        assert_eq!(init.build_cmd, vec!["make", "CFLAGS=-O2", "all"]);
    }

    #[test]
    fn init_requires_build_cmd() {
        let result = Cli::try_parse_from(["c2rust-demo", "init"]);
        assert!(result.is_err());
    }
}
