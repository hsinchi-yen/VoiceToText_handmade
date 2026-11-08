#[derive(clap::Args)]
pub struct Args {
    /// Stop after this many seconds.
    #[arg(long, default_value_t = 60)]
    pub seconds: u64,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("hotkey-test is not implemented yet (task T0.2)")
}
