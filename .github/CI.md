# CI

Linux checks and release coordination use `kalana-ci`.
macOS jobs use `[self-hosted, kalana-macos-ci, macOS]`. Windows x64 and ARM64 jobs
keep their original GitHub-hosted Windows runners, as explicitly requested by the
repository owner because no self-hosted Windows runner is available.

Trusted pull requests and master pushes run the existing Linux/macOS/Windows test
matrix, formatting, MSRV, and Clippy checks. Clippy is now blocking instead of
continue-on-error. Additional jobs compile the lite/medium feature tiers and build
and smoke-test the optimized CLI before validating crate packaging. Rust builds
use the committed Cargo.lock. Packaging intentionally resolves the registry form
of git dependencies, matching cargo publish's manifest rewrite.

Linux release binaries for both x64 and ARM64 are cross-compiled with pinned Zig
and cargo-zigbuild, preserving the previous Ubuntu 22.04/glibc 2.35 release floor
without requiring a second native Linux architecture runner. macOS and Windows
builds preserve their native toolchains, signing, archives, checksums, and release
publication gates. General jobs bootstrap GitHub CLI without sudo or apt.

The beta release channel stays disabled. Its job guards now use the
BETA_CHANNEL_ENABLED repository variable because job-level expressions cannot
access env. An unset variable is false; re-enabling the channel still requires
restoring its release trigger as described in release-beta.yml.

All jobs are bounded by timeouts; unreviewed external forks cannot execute the
self-hosted test matrix. After review, a maintainer can copy a contribution to a
branch in this repository to run the full checks. Runner-list APIs return HTTP
403 for this integration, so runner registration and repository access are
provisioned externally. The Mac release runner needs Apple Silicon, the macOS
SDK, and the deployment-target support already required by the existing build.
