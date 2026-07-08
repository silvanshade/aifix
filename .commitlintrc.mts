import { makeConfig } from "./.agents/core/fragments/commitlintrc.base.mts";

// Fixed scope vocabulary for aifix-specific surfaces. The agentic-dev base
// carries shared conventional-commit, attribution, and publishable-history
// rules; only project-specific scopes belong here.
export default makeConfig([
  "agda",
  "batch",
  "cache",
  "cargo",
  "cli",
  "mcp",
  "mise",
  "release",
  "treefmt",
]);
