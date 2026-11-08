use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Directory with `<name>.wav` + `<name>.txt` pairs.
    pub dir: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Optional LLM for polish: lmstudio | <cloud id>.
    #[arg(long)]
    pub llm: Option<String>,
    /// Model file for local providers.
    #[arg(long)]
    pub model: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("bench is not implemented yet (task T0.1)")
}
