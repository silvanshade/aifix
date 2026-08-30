// Real trailer tokens. The conventional-commits parser treats ANY `word:` line
// as the footer start, so the stock footer-leading-blank rule misfires on
// wrapped prose; this closed list is what makes the replacement rule sound.
const TRAILER_TOKENS = [
  "BREAKING CHANGE",
  "BREAKING-CHANGE",
  "Acked-by",
  "Cc",
  "Closes",
  "Co-Authored-By",
  "Fixes",
  "Refs",
  "Reported-by",
  "Reviewed-by",
  "Session",
  "Signed-off-by",
  "Tested-by",
];
const TRAILER_LINE = new RegExp(`^(?:${TRAILER_TOKENS.join("|")}):[ \t]`, "i");

const trailerLeadingBlank = (parsed) => {
  const raw = parsed.raw ?? [parsed.header, parsed.body, parsed.footer].filter(Boolean).join("\n");
  const lines = raw.split("\n");
  const first = lines.findIndex((line) => TRAILER_LINE.test(line.trimEnd()));
  if (first <= 0) return [true, ""];
  if ((lines[first - 1] ?? "").trim() === "") return [true, ""];
  return [
    false,
    `the trailer block must be preceded by a blank line; found "${(lines[first] ?? "").trim()}"`,
  ];
};

// Closed vocabulary. Grow deliberately; per-surface growth is the failure mode.
//
// Subsystem scopes name the surfaces the single-crate workspace exposes (CLI,
// MCP, batch runner, the agda face, and core); infra scopes name the surfaces
// that carry no crate. Every scope here is in use in this repo's history.
const SCOPES = [
  "adr",
  "agda",
  "batch",
  "changelog",
  "ci",
  "cli",
  "config",
  "core",
  "mcp",
  "repo",
];

const BANNED_TRAILERS = /^(Entire-Checkpoint|Claude-Session|Codex-Session|Gpt-Session):/im;

const sessionTrailerRequired = (parsed) => {
  const raw = parsed.raw ?? "";
  if (BANNED_TRAILERS.test(raw))
    return [false, "plaintext agent-session trailers are banned (opaque Session tokens only)"];
  // Every Session line must be opaque — one valid line must not mask a
  // malformed or plaintext sibling. Multiple valid lines stay legal: the
  // merge queue's squash concatenates branch commit messages.
  const sessions = raw.split("\n").filter((line) => /^Session:/i.test(line.trimEnd()));
  if (sessions.length === 0)
    return [false, "commit needs an opaque Session trailer (mint one, export GANDR_SESSION_TOKEN)"];
  const bad = sessions.find((line) => !/^Session: 1\.[A-Za-z0-9_-]+$/.test(line.trimEnd()));
  if (bad) return [false, `malformed Session trailer "${bad.trim()}": opaque form required`];
  return [true, ""];
};

export default {
  extends: ["@commitlint/config-conventional"],
  plugins: [
    {
      rules: {
        "trailer-leading-blank": trailerLeadingBlank,
        "session-trailer-required": sessionTrailerRequired,
      },
    },
  ],
  rules: {
    "header-max-length": [2, "always", 72],
    "header-trim": [2, "always"],
    "subject-empty": [2, "never"],
    "subject-full-stop": [2, "never", "."],
    "body-leading-blank": [2, "always"],
    "body-max-line-length": [2, "always", 100],
    // Disabled: the conventional-commits parser reclassifies wrapped prose
    // bodies as footer whenever a line starts with `word:`; the custom
    // trailer-leading-blank rule above is the sound replacement.
    "footer-leading-blank": [0, "always"],
    "trailer-leading-blank": [2, "always"],
    "session-trailer-required": [2, "always"],
    "type-empty": [2, "never"],
    "scope-empty": [2, "never"],
    "scope-case": [2, "always", "lower-case"],
    "scope-enum": [2, "always", SCOPES],
  },
};
