//! CLI entry: pdf-wm-remover
//!
//! Usage:
//!   pdf-wm-remover analyze input.pdf [--ratio 0.3] [--min-pages 2]
//!   pdf-wm-remover remove input.pdf -o output.pdf -k "keyword" [-k "kw2" ...]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pdf_wm_remover::{analyze, load_document, remove_watermarks};

#[derive(Parser)]
#[command(name = "pdf-wm-remover", version, about = "PDF watermark remover (content-stream level)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze a PDF and list repeated text candidates (potential watermarks)
    Analyze {
        /// Input PDF path
        input: PathBuf,
        /// Ratio of pages a text must appear on to be a candidate (0..=1)
        #[arg(long, default_value_t = 0.3)]
        ratio: f64,
        /// Minimum number of pages for a candidate regardless of ratio
        #[arg(long, default_value_t = 2)]
        min_pages: usize,
        /// Password for encrypted PDF (default: try empty)
        #[arg(long)]
        password: Option<String>,
    },
    /// Remove watermark text blocks matching keywords and strip permissions
    Remove {
        /// Input PDF path
        input: PathBuf,
        /// Output PDF path
        #[arg(short, long)]
        output: PathBuf,
        /// Watermark keyword (repeatable; case-insensitive substring match)
        #[arg(short = 'k', long = "keyword", required = true)]
        keywords: Vec<String>,
        /// Password for encrypted PDF (default: try empty)
        #[arg(long)]
        password: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Analyze {
            input,
            ratio,
            min_pages,
            password,
        } => {
            let doc = load_document(&input, password.as_deref())?;
            let total = doc.get_pages().len();
            println!("{}: {} pages", input.display(), total);
            let candidates = analyze(&doc, ratio, min_pages)?;
            if candidates.is_empty() {
                println!("No candidates found (threshold: max({:.0}%, {} pages)).", ratio * 100.0, min_pages);
                return Ok(());
            }
            println!("\n{:<8} {:<6}  {}", "count", "size", "text");
            println!("{}", "-".repeat(72));
            for c in &candidates {
                let text: String = c.text.chars().take(60).collect();
                println!("{:<8} {:<6.1}  {}", c.count, c.size, text);
            }
            println!("\nTip: use `remove` with -k '<text>' to delete matching blocks.");
        }
        Commands::Remove {
            input,
            output,
            keywords,
            password,
        } => {
            let report = remove_watermarks(&input, &output, &keywords, password.as_deref())?;
            println!("input : {}", input.display());
            println!("output: {}", output.display());
            println!("pages : {} (touched {})", report.total_pages, report.pages_touched);
            println!("removed text blocks: {}", report.removed_blocks);
            if report.removed_blocks == 0 {
                eprintln!("WARNING: no blocks matched. Check keywords with `analyze` first.");
            }
        }
    }
    Ok(())
}