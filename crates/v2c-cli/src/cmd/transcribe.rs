use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// 16 kHz mono WAV file.
    pub wav: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Model file for local providers.
    #[arg(long)]
    pub model: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("transcribe is not implemented yet (task T0.1)")
}
