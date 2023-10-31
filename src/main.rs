mod common;
mod search;
mod diff;
mod contains;
mod link;

use argh::FromArgs;

/// Cross-platform Symbol Tools
#[derive(FromArgs, Debug)]
struct Options {
    #[argh(subcommand)]
    command: Command
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum Command {
    Search(search::Options),
    Diff(diff::Options),
    Contains(contains::Options),
    Link(link::Options)
}

fn main() -> anyhow::Result<()> {
    let options: Options = argh::from_env();

    match options.command {
        Command::Search(cmd) => cmd.exec(),
        Command::Diff(cmd) => cmd.exec(),
        Command::Contains(cmd) => cmd.exec(),
        Command::Link(cmd) => cmd.exec()
    }
}
