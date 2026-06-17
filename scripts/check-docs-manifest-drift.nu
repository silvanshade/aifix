#!/usr/bin/env nu

# Verify docs/MANIFEST.toml BLAKE3 hashes against the current working tree.
# Expected manifest shape:
#
# [[files]]
# path = "docs/example.md"
# b3 = "0123...64 lowercase hex characters...abcd"

const hash_pattern = r#'^[0-9a-f]{64}$'#

# Check every docs manifest entry for missing files, invalid hashes, and drift.
def main [
    --manifest (-m): path = docs/MANIFEST.toml # Manifest file to verify.
    ...paths: path # Optional manifest path supplied by treefmt.
]: nothing -> nothing {
    let root = (repo-root)
    let selected_manifest = if ($paths | is-empty) { $manifest } else { $paths | first }
    let manifest_path = (resolve-under-root $root $selected_manifest)
    if not ($manifest_path | path exists) {
        error make {msg: $'docs manifest not found: ($manifest_path)'}
    }

    let manifest_data = (try { open $manifest_path } catch {|err| error make {msg: $'failed to read docs manifest ($manifest_path): ($err.msg)'} })
    let entries = (manifest-entries $manifest_data)
    if ($entries | is-empty) {
        error make {msg: $'docs manifest contains no [[files]] entries: ($manifest_path)'}
    }

    let manifest_dir = ($manifest_path | path dirname)
    let failures = ($entries | each {|entry| verify-entry $root $manifest_dir $entry } | where status != 'ok')

    if ($failures | is-empty) {
        print $'docs manifest drift check OK: verified ($entries | length) files from ($manifest_path | path relative-to $root)'
    } else {
        print --stderr $'docs manifest drift check FAILED: ($failures | length) manifest entries are invalid, missing, or drifted'
        $failures | each {|failure| print --stderr (format-failure $failure) }
        exit 1
    }
}

# Return the current repository root, or fail with a clear worktree error.
def repo-root []: nothing -> path {
    let root = (^git rev-parse --show-toplevel | complete)
    if $root.exit_code != 0 {
        error make {msg: ($'not a git worktree: ($root.stderr | str trim)' | str trim)}
    }

    $root.stdout | str trim
}

# Resolve a user path below the repository root.
def resolve-under-root [root: path, candidate: path]: nothing -> path {
    let expanded = if (($candidate | into string) | str starts-with '/') {
        $candidate | path expand
    } else {
        $root | path join $candidate | path expand
    }
    let root_expanded = ($root | path expand)
    let expanded_text = ($expanded | into string)
    let root_text = ($root_expanded | into string)
    let root_prefix = $'($root_text)/'

    if not (($expanded_text == $root_text) or ($expanded_text | str starts-with $root_prefix)) {
        error make {msg: $'path escapes repository root: ($candidate)'}
    }

    $expanded
}

# Extract manifest entries from [[files]] or [files."path"] forms.
def manifest-entries [manifest_data: record]: nothing -> list<record<path: string, b3: string>> {
    let files = ($manifest_data.files? | default [])
    let files_type = ($files | describe)

    if ($files_type | str starts-with 'record') {
        $files
        | transpose path entry
        | each {|row|
            {
                path: ($row.entry.path? | default $row.path | into string)
                b3: ($row.entry.b3? | default ($row.entry.hash? | default '') | into string)
            }
        }
    } else {
        $files
        | each {|entry|
            {
                path: ($entry.path? | default '' | into string)
                b3: ($entry.b3? | default ($entry.hash? | default '') | into string)
            }
        }
    }
}

# Verify one manifest entry and return a status record.
def verify-entry [
    root: path
    manifest_dir: path
    entry: record<path: string, b3: string>
]: nothing -> record<path: string, status: string, expected: string, actual: string> {
    let expected = ($entry.b3 | str trim)
    if ($entry.path | str trim | is-empty) {
        {path: '<empty>', status: 'invalid', expected: $expected, actual: 'manifest entry has an empty path'}
    } else if not ($expected =~ $hash_pattern) {
        {path: $entry.path, status: 'invalid', expected: $expected, actual: 'expected b3 must be a 64-character lowercase BLAKE3 digest'}
    } else {
        let file = (resolve-entry-path $root $manifest_dir $entry.path)
        if not ($file | path exists) {
            {path: $entry.path, status: 'missing', expected: $expected, actual: 'file missing'}
        } else if (($file | path type) != 'file') {
            {path: $entry.path, status: 'invalid', expected: $expected, actual: 'manifest path is not a regular file'}
        } else {
            let actual = (hash-file $file)
            let status = if $actual == $expected { 'ok' } else { 'drift' }
            {path: $entry.path, status: $status, expected: $expected, actual: $actual}
        }
    }
}

# Resolve an entry relative to repo root first, then the manifest directory.
def resolve-entry-path [root: path, manifest_dir: path, entry_path: string]: nothing -> path {
    if (($entry_path | into string) | str starts-with '/') {
        error make {msg: $'manifest paths must be repository-relative, got absolute path: ($entry_path)'}
    }

    let root_candidate = ($root | path join $entry_path | path expand)
    let manifest_candidate = ($manifest_dir | path join $entry_path | path expand)

    if ($root_candidate | path exists) {
        resolve-under-root $root $root_candidate
    } else {
        resolve-under-root $root $manifest_candidate
    }
}

# Compute a BLAKE3 digest with b3sum and validate the command output shape.
def hash-file [file: path]: nothing -> string {
    let hashed = (^b3sum --no-names $file | complete)
    if $hashed.exit_code != 0 {
        error make {msg: ($'b3sum failed for ($file): ($hashed.stderr | str trim)' | str trim)}
    }

    let digest = ($hashed.stdout | str trim)
    if not ($digest =~ $hash_pattern) {
        error make {msg: $'b3sum returned a malformed digest for ($file): ($digest)'}
    }

    $digest
}

# Render one failure as a compact, grep-friendly diagnostic block.
def format-failure [failure: record<path: string, status: string, expected: string, actual: string>]: nothing -> string {
    $'($failure.path): ($failure.status) expected=($failure.expected) actual=($failure.actual)'
}
