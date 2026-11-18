use clap::Parser;

#[derive(Parser)]
#[command(name = maild_brand::LAUNCHER_BIN, version, about = "原生修复与后台管理")]
struct Arguments {
    /// Open the native recovery window without opening a mailbox.
    #[arg(long)]
    repair: bool,
}

fn main() {
    let args = Arguments::parse();
    maild_launcher::run(args.repair);
}
