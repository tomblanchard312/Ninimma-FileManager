# Contributing to Ninimma

Ideas, bug reports, and focused pull requests are welcome. Open an issue before a
large change so we can agree on scope. No repository write access is needed:
fork this repository, make a branch in your fork, and submit a pull request.

Explain the user problem, proposed behavior, and any alternatives. For bugs, give
the platform, version, reproduction steps, and logs stripped of personal data.
Use made-up files in screenshots and tests. Report vulnerabilities privately as
explained in [SECURITY.md](SECURITY.md).

Keep changes focused. Preserve licenses and attribution. Contributions are under
the repository's Apache-2.0 license; submit only work you have permission to share.
Prefer Rust with egui/eframe, ordinary-user operation, and explicit native
approval for privileged operations. Protect user files and settings.

Run `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`, `cargo test --locked --no-default-features`, and
`python3 -m unittest discover -s packaging/linux` when applicable. Describe the
checks actually run and any platform you could not test.

The owner reviews contributions. Main requires a pull request, owner review,
passing checks, and resolved conversations. New commits dismiss stale approvals.
Administrators retain a pull-request-only bypass for owner-authored changes and
recovery; bypassing a failing security check requires an explicit explanation.
Release tags are restricted to administrators and protected against changes.
Outside contributors need maintainer approval before their workflows can run.
Do not request write/admin access just to submit an idea or patch.

Be respectful and constructive. Harassment, threats, spam, and disclosure of
private information are not welcome. The owner may moderate content, close
requests, or block abusive accounts. Participation does not guarantee acceptance.

When repository variable `NINIMMA_LOCAL_RUNNER` is `true`, Linux checks for
trusted repository branches and release FreeBSD VM builds run in the isolated
local VM. Otherwise they use standard hosted runners, which are free for this
public repository. This keeps required checks runnable during local maintenance. Fork pull requests use standard hosted runners; do not
route untrusted fork code to the local runner. Native Windows/macOS tests use
standard hosted runners, which are free for this public repository.
