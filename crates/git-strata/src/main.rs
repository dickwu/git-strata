//! `git-strata` (and `git strata`, D1): one compact JSON envelope on stdout.

fn main() {
    std::process::exit(git_strata::run());
}
