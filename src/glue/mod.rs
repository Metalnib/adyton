//! Shell glue (spec §5): per-shell scripts embedded at compile time (D7) and
//! emitted by `adyton init <shell>` with this binary's own path baked in, so
//! the glue works without adyton on PATH.

use crate::cli::Shell;

const ZSH: &str = include_str!("adyton.zsh");
const BASH: &str = include_str!("adyton.bash");
const FISH: &str = include_str!("adyton.fish");

pub fn init(shell: Shell) -> String {
    let template = match shell {
        Shell::Zsh => ZSH,
        Shell::Bash => BASH,
        Shell::Fish => FISH,
    };
    render(template)
}

fn render(template: &str) -> String {
    let exe =
        std::env::current_exe().map_or_else(|_| "adyton".to_owned(), |p| p.display().to_string());
    template.replace("{{ADYTON}}", &exe)
}
