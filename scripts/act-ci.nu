#!/usr/bin/env nu

# Run the GitHub Actions workflow through act with project-local cache mounts.
#
# The workflow owns concrete job command lists. This wrapper only selects jobs
# and supplies act with persistent Docker volumes for Cargo, rustup, mise, and
# aube caches. It intentionally avoids `--bind`: act copies source, while heavy
# mutable state lives in named volumes mounted at the paths workflow steps use.
# Cache setup is mount-only. If a future local cache seeding step must copy an
# artifact tree, use mise's pinned uutils binary:
# `mise exec -- coreutils cp --reflink=auto -a`, never platform `/bin/cp`.
use std/assert

const ACT_VOLUME_PREFIX = 'aifix-act'
const ACT_TARGET_ROOT = '/tmp/aifix-act-target'
const DEFAULT_WORKFLOW_PATH = '.github/workflows/ci.yml'
const DEFAULT_JOB_ORDER = [
    'template'
    'docs-gates'
    'project-lint'
    'cargo-build'
    'cargo-clippy'
    'cargo-nextest'
    'cargo-llvm-cov'
]

def runner-path [segments: list<string>]: nothing -> string {
    (['/' 'home' 'runner'] ++ $segments | path join)
}

def act-target-dir []: nothing -> string {
    $ACT_TARGET_ROOT | path join (pwd | path basename)
}

def cache-mounts []: nothing -> list<record<source: string, target: string>> {
    [
        {
            source: $'($ACT_VOLUME_PREFIX)-cargo-registry'
            target: (runner-path ['.cargo' 'registry'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-cargo-git'
            target: (runner-path ['.cargo' 'git'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-target'
            target: $ACT_TARGET_ROOT
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-rustup'
            target: (runner-path ['.rustup'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-mise'
            target: (runner-path ['.local' 'share' 'mise'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-mise-state'
            target: (runner-path ['.local' 'state' 'mise'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-mise-cache'
            target: (runner-path ['.cache' 'mise'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-aube-store'
            target: (runner-path ['.local' 'share' 'aube'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-aube-cache'
            target: (runner-path ['.cache' 'aube'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-cargo-careful-cache'
            target: (runner-path ['.cache' 'cargo-careful'])
        }
        {
            source: $'($ACT_VOLUME_PREFIX)-tree-sitter-cache'
            target: (runner-path ['.cache' 'tree-sitter'])
        }
    ]
}

# Docker option fragment for named cache volumes.
def cache-volume-options []: nothing -> string {
    cache-mounts
    | each {|mount| $'-v ($mount.source):($mount.target)' }
    | str join ' '
}

# Worktree checkouts use a .git file pointing outside the worktree. Mounting the
# common git dir at the same absolute path lets act containers read history.
# The primary checkout has a real .git directory inside the copied workspace, so
# mounting it read-only would make act's checkout copy fail.
def git-common-dir-option []: nothing -> string {
    if (((pwd | path join '.git') | path type) == 'dir') {
        return ''
    }

    let result = (^git rev-parse --path-format=absolute --git-common-dir | complete)
    if $result.exit_code != 0 {
        return ''
    }

    let git_dir = ($result.stdout | str trim)
    if (($git_dir | is-empty) or not ($git_dir | path exists)) {
        return ''
    }

    $'-v ($git_dir):($git_dir):ro'
}

# Compose Docker container options as one string because act forwards the value
# directly to Docker.
def container-options []: nothing -> string {
    [
        (cache-volume-options)
        (git-common-dir-option)
    ]
    | where ($it | is-not-empty)
    | str join ' '
}

def act-base-args [mise_env: string, workflow_path: path]: nothing -> list<string> {
    let options = (container-options)
    assert ($options =~ $'($ACT_VOLUME_PREFIX)-target') 'act-ci: target cache mount missing'
    assert ($options =~ $'($ACT_VOLUME_PREFIX)-cargo-registry') 'act-ci: Cargo registry cache mount missing'

    [
        '-W'
        ($workflow_path | into string)
        '--use-gitignore'
        '--action-offline-mode'
        '--pull=false'
        $'--env=MISE_ENV=($mise_env)'
        $'--env=CARGO_TARGET_DIR=(act-target-dir)'
        $'--container-options=($options)'
    ]
}

def workflow-jobs [workflow_path: path]: nothing -> list<string> {
    let workflow = try {
        open $workflow_path
    } catch {|err|
        error make {msg: $'act-ci: failed to read ($workflow_path): ($err.msg)'}
    }

    $workflow | get jobs | columns
}

def ordered-workflow-jobs [workflow_path: path]: nothing -> list<string> {
    let jobs = (workflow-jobs $workflow_path)
    let ordered = ($DEFAULT_JOB_ORDER | where ($it in $jobs))
    let extras = ($jobs | where ($it not-in $DEFAULT_JOB_ORDER))

    $ordered ++ $extras
}

def run-act [act_args: list<string>]: nothing -> nothing {
    ^act ...$act_args
}

def main [
    --workflow (-w): path = $DEFAULT_WORKFLOW_PATH # Workflow file to inspect and run.
    --mise-env: string = 'ci'                      # mise environment selected in the workflow.
    --job (-j): string = ''                        # CI job id to run through act.
    --list (-l)                                    # List available act jobs.
    --all                                          # Run every workflow job sequentially, cheapest known jobs first.
    --rm                                           # Remove containers/volumes after failed runs.
    ...args: string                                # Additional act arguments.
]: nothing -> nothing {
    assert (not ($all and $list)) 'act-ci: --all and --list are mutually exclusive'
    assert (not ($all and ($job | is-not-empty))) 'act-ci: --all and --job are mutually exclusive'

    let base_args = (act-base-args $mise_env $workflow)
    let rm_args: list<string> = if $rm { ['--rm'] } else { [] }

    if $all {
        for ci_job in (ordered-workflow-jobs $workflow) {
            print $'act-ci: running ($ci_job)'
            run-act ($base_args ++ ['--job' $ci_job] ++ $rm_args ++ $args)
        }
        return
    }

    let list_args: list<string> = if $list { ['--list'] } else { [] }
    let job_args: list<string> = if ($job | is-empty) { [] } else { ['--job' $job] }
    run-act ($base_args ++ $list_args ++ $job_args ++ $rm_args ++ $args)
}
