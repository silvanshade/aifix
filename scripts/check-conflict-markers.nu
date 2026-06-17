#!/usr/bin/env nu

# Reject unresolved Git conflict markers in tracked text-like files.
# The gate never rewrites files; it reports every marker-shaped line it finds.

const marker_pattern = r#'^(<<<<<<<|=======|>>>>>>>|\|\|\|\|\|\|\|)'#
const text_extensions = [
    css
    h
    hpp
    html
    js
    json
    lock
    md
    nu
    rs
    sh
    toml
    ts
    txt
    yaml
    yml
]
const text_names = [
    .gitignore
    AGENTS.md
    Cargo.lock
    Cargo.toml
    LICENSE
    README.md
]
const ignored_prefixes = [
    .beads/
    .git/
    build/
    coverage/
    dist/
    target/
    temp/
    tmp/
    vendor/
]

# Scan tracked text-like files, or explicit text-like files passed by treefmt.
def main [
    ...paths: path # Files to scan; defaults to git-tracked text-like files.
]: nothing -> nothing {
    let scan_paths = if ($paths | is-empty) { tracked-text-files } else { $paths | where {|file| is-text-like $file } }
    let findings = ($scan_paths | each {|file| scan-file $file } | flatten)

    if ($findings | is-empty) {
        print $'conflict marker check OK: no unresolved markers found in ($scan_paths | length) text-like files'
    } else {
        print --stderr 'conflict marker check FAILED: unresolved Git conflict markers found'
        $findings | each {|finding|
            print --stderr $'($finding.path):($finding.line):($finding.text)'
        }
        exit 1
    }
}

# Return tracked files that are safe and useful to scan as text.
def tracked-text-files []: nothing -> list<path> {
    let root = (repo-root)
    let listed = (^git -C $root ls-files | complete)
    if $listed.exit_code != 0 {
        error make {msg: ($'git ls-files failed: ($listed.stderr | str trim)' | str trim)}
    }

    $listed.stdout
    | lines
    | where ($it | is-not-empty)
    | where {|relative_path| not (is-ignored-path $relative_path) }
    | where {|relative_path| is-text-like $relative_path }
    | each {|relative_path| $root | path join $relative_path }
    | where {|file| ($file | path exists) and (($file | path type) == 'file') }
}

# Return the current repository root, or fail with a clear worktree error.
def repo-root []: nothing -> path {
    let root = (^git rev-parse --show-toplevel | complete)
    if $root.exit_code != 0 {
        error make {msg: ($'not a git worktree: ($root.stderr | str trim)' | str trim)}
    }

    $root.stdout | str trim
}

# Decide whether a path belongs to a text-like source/config/document class.
def is-text-like [file: path]: nothing -> bool {
    let parsed = ($file | path parse)
    let extension = ($parsed.extension? | default '' | str downcase)
    let basename = ($file | path basename)

    ($basename in $text_names) or ($extension in $text_extensions)
}

# Exclude generated, vendored, local, and bd-owned trees from default scans.
def is-ignored-path [relative_path: string]: nothing -> bool {
    $ignored_prefixes | any {|prefix| $relative_path | str starts-with $prefix }
}

# Return marker findings for one regular file.
def scan-file [file: path]: nothing -> list<record<path: path, line: int, text: string>> {
    if not ($file | path exists) {
        error make {msg: $'file not found: ($file)'}
    }

    if (($file | path type) != 'file') {
        []
    } else {
        let display_path = try { $file | path relative-to (pwd) } catch { $file }
        let body = (try { open --raw $file } catch {|err| error make {msg: $'failed to read ($file): ($err.msg)'} })

        $body
        | lines
        | enumerate
        | where item =~ $marker_pattern
        | each {|entry| {path: $display_path, line: ($entry.index + 1), text: $entry.item} }
    }
}
