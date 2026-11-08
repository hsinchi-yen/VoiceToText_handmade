#[derive(clap::Args)]
pub struct Args {
    /// Text to inject; `\n` is a line break.
    pub text: String,
    /// Seconds to wait so you can focus the target window.
    #[arg(long, default_value_t = 3)]
    pub delay: u64,
    /// Force clipboard paste instead of typing (T1.2).
    #[arg(long)]
    pub paste: bool,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("inject-test is not implemented yet (task T0.3)")
}
