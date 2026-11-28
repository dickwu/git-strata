//! `git-strata` (and `git strata`, D1): one compact JSON envelope on stdout.

strata_types::mark_release_binary!();

fn main() {
    std::process::exit(git_strata::run());
}
