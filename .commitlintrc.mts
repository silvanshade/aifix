import { type RulesConfig } from "@commitlint/types";

type Config = {
  extends: string[];
  rules: Partial<RulesConfig>;
};

export const config: Config = {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "body-max-line-length": [2, "always", 100],
    "header-trim": [2, "always"],
    "subject-case": [2, "always", ["lower-case"]],
    "scope-enum": [
      2,
      "always",
      {
        scopes: [
          "cache",
          "cargo",
          "ci",
          "config",
          "docs",
          "git",
          "mcp",
          "mise",
          "release",
          "treefmt",
        ],
        delimiters: [","],
      },
    ],
    "subject-empty": [2, "never"],
    "type-empty": [2, "never"],
  },
};

export default config;
