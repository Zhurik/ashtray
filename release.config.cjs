/**
 * @type {import("semantic-release").GlobalConfig}
 */

// Where the image goes is configured in the repository, not here: the release
// workflow passes these in from GitHub Actions variables.
const dockerRegistry = process.env.DOCKER_REGISTRY || null;
const dockerProject = process.env.DOCKER_PROJECT || null;

module.exports = {
  branches: ["main"],
  ci: true,
  plugins: [
    "@semantic-release/commit-analyzer",
    "@semantic-release/release-notes-generator",
    "@semantic-release/changelog",
    [
      // The crate version is the one the binary reports, so it follows the
      // release. `cargo metadata` rewrites the crate's own entry in Cargo.lock
      // without touching any dependency.
      "@semantic-release/exec",
      {
        prepareCmd:
          "sed -i 's/^version = \".*\"/version = \"${nextRelease.version}\"/' Cargo.toml && cargo metadata --format-version 1 > /dev/null",
      },
    ],
    [
      "@codedependant/semantic-release-docker",
      {
        dockerRegistry,
        dockerProject,
        dockerImage: "ashtray",
        dockerTags: ["latest", "{{version}}", "{{major}}", "{{major}}.{{minor}}"],
        dockerPlatform: ["linux/amd64", "linux/arm64"],
      },
    ],
    [
      "@semantic-release/git",
      {
        assets: ["CHANGELOG.md", "Cargo.toml", "Cargo.lock"],
      },
    ],
    "@semantic-release/github",
  ],
};
