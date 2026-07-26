//! Bounded one-shot LSP code-action execution for batch mutation modes.
//!
//! The module owns the language-server lifecycle, diagnostic correlation,
//! automatic-action safety policy, workspace-edit application, and convergence
//! bound behind one profile-oriented interface.

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString as _;
use alloc::sync::Arc;
use alloc::vec::Vec;
#[cfg(test)]
use core::sync::atomic::AtomicU64;
#[cfg(test)]
use core::sync::atomic::Ordering;
use core::time::Duration;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read as _;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::Child;
use std::process::ChildStdin;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::time::Instant;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde_json::Value;
use serde_json::json;

use crate::adapter::parse_lsp_value;
use crate::config::ProfileConfig;
use crate::error::AifixError;
use crate::model::Diagnostic;

/// Built-in profile name with an implicit rust-analyzer capability.
const RUST_PROFILE: &str = "rust";
/// Default language-server executable for the built-in Rust profile.
const RUST_ANALYZER_COMMAND: &str = "rust-analyzer";
/// Default bound on successful action applications.
const DEFAULT_MAX_ITERATIONS: usize = 64;
/// Default per-session and per-request timeout in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Maximum accepted complete-session timeout in milliseconds.
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;
/// Idle interval used to infer the end of diagnostic publication.
const DIAGNOSTIC_IDLE_MS: u64 = 300;
/// Retry bound for transient LSP content-modified responses.
const MAX_CONTENT_MODIFIED_RETRIES: usize = 3;
/// Maximum accepted JSON-RPC message payload.
const MAX_LSP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum retained language-server stderr suffix.
const MAX_LSP_STDERR_BYTES: usize = 64 * 1024;
/// Maximum aggregate bytes accepted before an LSP message payload.
const MAX_LSP_HEADER_BYTES: usize = 16 * 1024;
/// Maximum queued server events before stdout backpressure applies.
const MAX_PENDING_LSP_EVENTS: usize = 1;
/// Maximum responses deferred while servicing server traffic during writes.
const MAX_DEFERRED_LSP_EVENTS: usize = 8;
/// Maximum server messages accepted during one language-server session.
const MAX_SERVER_MESSAGES: usize = 0x0001_0000;
/// Maximum nested server requests serviced while client writes are pending.
const MAX_NESTED_SERVER_REQUESTS: usize = 32;
/// Maximum aggregate decoded bytes retained while writes are pending.
const MAX_DEFERRED_LSP_BYTES: usize = 64 * 1024 * 1024;
/// Maximum queued client writes before producer backpressure applies.
const MAX_PENDING_LSP_WRITES: usize = 8;
/// Maximum diagnostic-correlated action requests in one session.
const MAX_ACTION_QUERIES: usize = 16 * 1024;
/// Maximum code actions accepted in one response before selection.
const MAX_ACTIONS_PER_RESPONSE: usize = 64 * 1024;
/// Maximum published diagnostics retained across one session.
const MAX_PUBLISHED_DIAGNOSTICS: usize = 64 * 1024;
/// Maximum estimated bytes retained for residual and actionable diagnostics.
const MAX_RETAINED_DIAGNOSTIC_BYTES: usize = 64 * 1024 * 1024;
/// Maximum text edits accepted in one atomic workspace transaction.
const MAX_WORKSPACE_TEXT_EDITS: usize = 64 * 1024;
/// Maximum aggregate bytes retained by successful action loop keys.
const MAX_RETAINED_ACTION_KEY_BYTES: usize = 64 * 1024 * 1024;
/// Maximum aggregate bytes retained while selecting one response's actions.
const MAX_ACTION_CANDIDATE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum aggregate extended-attribute bytes copied for one source file.
const MAX_SECURITY_METADATA_BYTES: usize = 16 * 1024 * 1024;
/// Maximum aggregate bytes retained by staged or journaled workspace edits.
const MAX_STAGED_WORKSPACE_EDIT_BYTES: usize = 256 * 1024 * 1024;
/// Maximum document URIs retained from diagnostic publications.
const MAX_DIAGNOSTIC_DOCUMENTS: usize = MAX_SOURCE_FILES;
/// Maximum number of source documents opened in one session.
const MAX_SOURCE_FILES: usize = 4096;
/// Maximum directory entries visited while discovering source documents.
const MAX_DISCOVERY_ENTRIES: usize = 256 * 1024;
/// Maximum entries collected for deterministic ordering in one directory.
const MAX_DIRECTORY_ENTRIES: usize = 64 * 1024;
/// Maximum directories queued while discovering source documents.
const MAX_DISCOVERY_DIRECTORIES: usize = 64 * 1024;
/// Maximum aggregate path bytes retained during source discovery.
const MAX_DISCOVERY_PATH_BYTES: usize = 64 * 1024 * 1024;
/// Maximum aggregate bytes retained for synchronized source snapshots.
const MAX_OPEN_DOCUMENT_BYTES: usize = 256 * 1024 * 1024;
/// Conservative per-node overhead used to budget decoded JSON state.
const JSON_VALUE_OVERHEAD_BYTES: usize = 128;
/// Conservative per-map-entry overhead used for retained session state.
const MAP_ENTRY_OVERHEAD_BYTES: usize = 256;
/// Bound on collision retries when staging an atomic source replacement.
const MAX_TEMP_FILE_ATTEMPTS: usize = 64;
/// Process-local suffix source for atomic replacement files.
#[cfg(test)]
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Validated settings for one code-action session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCodeActionConfig
{
    /// Direct language-server argv.
    argv: Vec<String>,
    /// LSP language identifier sent for opened documents.
    language_id: String,
    /// Source-file extensions eligible for opening.
    extensions: Vec<String>,
    /// Hierarchical code-action kinds eligible for selection.
    action_kinds: Vec<String>,
    /// Exact server command identifiers eligible for command-scoped edits.
    allowed_commands: Vec<String>,
    /// Bound on successful action applications.
    max_iterations: usize,
    /// Bound on session startup, requests, and diagnostic refreshes.
    timeout: Duration,
}

/// Return whether a profile advertises an LSP code-action capability.
///
/// Built-in Rust advertises the rust-analyzer default unless explicitly
/// disabled. Other profiles advertise support only when enabled nested
/// `code_actions` configuration is present.
///
/// # Panics
///
/// This function does not panic.
#[must_use]
pub fn has_code_action_support(
    profile_name: &str,
    profile: Option<&ProfileConfig>,
) -> bool
{
    let configured = profile.and_then(|profile| profile.code_actions.as_ref());
    if configured.is_some_and(|config| config.enabled == Some(false)) {
        return false;
    }
    matches!(profile_name, RUST_PROFILE) || configured.is_some()
}

/// Return the executable family advertised for profile discovery.
///
/// The configured first argv element wins over Rust's built-in
/// `rust-analyzer` fallback. Incomplete configured argv intentionally returns
/// no family so profile validation can report the malformed capability.
///
/// # Panics
///
/// This function does not panic.
#[must_use]
pub fn code_action_command_family(
    profile_name: &str,
    profile: Option<&ProfileConfig>,
) -> Option<String>
{
    let configured = profile.and_then(|profile| profile.code_actions.as_ref());
    if configured.is_some_and(|config| config.enabled == Some(false)) {
        return None;
    }
    if let Some(argv) = configured.and_then(|config| config.argv.as_ref()) {
        return argv
            .first()
            .filter(|executable| !executable.is_empty())
            .cloned();
    }
    matches!(profile_name, RUST_PROFILE).then(|| RUST_ANALYZER_COMMAND.to_owned())
}

/// Validate and resolve one profile's optional code-action settings.
///
/// Rust receives conservative built-in defaults. Explicit nested fields
/// replace those defaults individually; a non-Rust profile without nested
/// configuration resolves to no capability.
///
/// # Errors
///
/// leading-dot extensions, command-action allowlists, empty values, or zero
/// execution bounds.
///
/// # Panics
///
/// This function does not panic.
pub fn resolve_code_action_config(
    profile_name: &str,
    profile: Option<&ProfileConfig>,
) -> Result<Option<ResolvedCodeActionConfig>, AifixError>
{
    let configured = profile.and_then(|profile| profile.code_actions.as_ref());
    if configured.is_some_and(|config| config.enabled == Some(false)) {
        return Ok(None);
    }
    if !matches!(profile_name, RUST_PROFILE) && configured.is_none() {
        return Ok(None);
    }

    let mut argv =
        matches!(profile_name, RUST_PROFILE).then(|| vec![RUST_ANALYZER_COMMAND.to_owned()]);
    let mut language_id = matches!(profile_name, RUST_PROFILE).then(|| "rust".to_owned());
    let mut extensions = matches!(profile_name, RUST_PROFILE).then(|| vec!["rs".to_owned()]);
    let mut action_kinds = Some(vec!["quickfix".to_owned()]);
    let mut allowed_commands = Vec::new();
    let mut max_iterations = DEFAULT_MAX_ITERATIONS;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;

    if let Some(config) = configured {
        if config.argv.is_some() {
            argv.clone_from(&config.argv);
        }
        if config.language_id.is_some() {
            language_id.clone_from(&config.language_id);
        }
        if config.extensions.is_some() {
            extensions.clone_from(&config.extensions);
        }
        if config.action_kinds.is_some() {
            action_kinds.clone_from(&config.action_kinds);
        }
        if let Some(commands) = config.allowed_commands.as_ref() {
            allowed_commands.clone_from(commands);
        }
        if let Some(configured_max) = config.max_iterations {
            max_iterations = configured_max;
        }
        if let Some(configured_timeout) = config.timeout_ms {
            timeout_ms = configured_timeout;
        }
    }

    let argv = require_nonempty_list(argv, profile_name, "code_actions.argv")?;
    if argv.first().is_none_or(String::is_empty) {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.argv requires a nonempty executable"
        )));
    }
    let language_id = language_id
        .filter(|language_id| !language_id.trim().is_empty())
        .ok_or_else(|| {
            AifixError::invalid_argument(format!(
                "profile `{profile_name}` code_actions.language_id is required"
            ))
        })?;
    let extensions = require_nonempty_list(extensions, profile_name, "code_actions.extensions")?;
    if extensions
        .iter()
        .any(|extension| extension.is_empty() || extension.starts_with('.'))
    {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.extensions must contain nonempty extensions without leading dots"
        )));
    }
    let action_kinds =
        require_nonempty_list(action_kinds, profile_name, "code_actions.action_kinds")?;
    if action_kinds.iter().any(String::is_empty) {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.action_kinds must not contain empty values"
        )));
    }
    if allowed_commands.iter().any(String::is_empty) {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.allowed_commands must not contain empty values"
        )));
    }
    allowed_commands.sort_unstable();
    allowed_commands.dedup();
    if max_iterations == 0 {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.max_iterations must be greater than zero"
        )));
    }
    if timeout_ms == 0 {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.timeout_ms must be greater than zero"
        )));
    }
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(AifixError::invalid_argument(format!(
            "profile `{profile_name}` code_actions.timeout_ms must not exceed {MAX_TIMEOUT_MS}"
        )));
    }

    Ok(Some(ResolvedCodeActionConfig {
        argv,
        language_id,
        extensions,
        action_kinds,
        allowed_commands,
        max_iterations,
        timeout: Duration::from_millis(timeout_ms),
    }))
}

/// Negotiate one language server's required automatic-action capabilities
/// without opening or mutating source documents.
///
/// # Errors
///
/// Returns typed platform, process, protocol, or capability errors.
pub fn preflight_code_actions(
    config: &ResolvedCodeActionConfig,
    cwd: &Utf8Path,
) -> Result<(), AifixError>
{
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        drop((config, cwd));
        Err(AifixError::invalid_argument(
            "automatic LSP workspace edits require Linux or macOS atomic file exchange",
        ))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut session = LspSession::start(config, cwd)?;
        if let Err(error) = session.negotiate() {
            session.terminate_child();
            return Err(error);
        }
        session.shutdown()
    }
}

/// Run one bounded LSP session and return its residual diagnostics.
///
/// The session owns process startup, initialization, source synchronization,
/// deterministic action application, diagnostic convergence, and shutdown.
///
/// # Errors
///
/// Returns typed argument, process, protocol, UTF-8, or filesystem errors when
/// the server contract cannot be completed safely. Shutdown failures are
/// returned when action processing itself succeeded.
///
/// # Panics
///
/// This function does not panic.
pub fn apply_code_actions(
    config: &ResolvedCodeActionConfig,
    cwd: &Utf8Path,
) -> Result<Vec<Diagnostic>, AifixError>
{
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        drop((config, cwd));
        Err(AifixError::invalid_argument(
            "automatic LSP workspace edits require Linux or macOS atomic file exchange",
        ))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut session = LspSession::start(config, cwd)?;
        session.initialize()?;
        let result = session.run();
        let shutdown = session.shutdown();
        match (result, shutdown) {
            | (Ok(diagnostics), Ok(())) => Ok(diagnostics),
            | (Err(error), _) | (Ok(_), Err(error)) => Err(session.rollback_committed_edits(error)),
        }
    }
}

/// Require a present, nonempty configuration list.
fn require_nonempty_list(
    value: Option<Vec<String>>,
    profile_name: &str,
    field: &str,
) -> Result<Vec<String>, AifixError>
{
    value.filter(|values| !values.is_empty()).ok_or_else(|| {
        AifixError::invalid_argument(format!(
            "profile `{profile_name}` {field} requires at least one value"
        ))
    })
}
/// Whether a server request identifier satisfies the JSON-RPC identifier shape.
fn json_rpc_request_id_is_valid(id: &Value) -> bool
{
    matches!(id, Value::String(_))
        || id
            .as_number()
            .is_some_and(|number| number.is_i64() || number.is_u64())
}

/// Source document kept synchronized with the language server.
#[derive(Debug)]
struct OpenDocument
{
    /// Canonical path inside the configured workspace.
    path: Utf8PathBuf,
    /// Last text sent to the language server.
    text: String,
    /// Monotonically increasing LSP document version.
    version: i64,
}

/// One fully validated and staged document change in a workspace transaction.
#[derive(Debug, Clone)]
struct PreparedWorkspaceEdit
{
    /// Canonical LSP document URI.
    uri: String,
    /// Canonical path replaced by the transaction.
    path: Utf8PathBuf,
    /// Synchronized content required immediately before replacement.
    expected: String,
    /// Fully edited replacement content.
    updated: String,
    /// Same-directory staged replacement until committed.
    temporary: Option<Utf8PathBuf>,
}

/// Parsed reader-thread event.
type ReaderEvent = Result<Value, String>;

/// Supported server-selected document synchronization strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextDocumentSync
{
    /// Replace the complete document text after each local mutation.
    Full,
    /// Send a ranged replacement covering the previous complete document.
    Incremental,
}

/// One framed write delegated to the supervised writer thread.
struct WriteRequest
{
    /// Serialized JSON-RPC payload without framing headers.
    payload: Vec<u8>,
    /// Per-write acknowledgement channel.
    result: mpsc::Sender<Result<(), String>>,
}

/// Matching response accepted by a pending client request.
enum ResponseOutcome
{
    /// Successful JSON-RPC result payload.
    Result(Value),
    /// Retryable LSP content-modified response.
    ContentModified,
}

/// Whether a command may submit its one scoped workspace edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandEditScope
{
    /// No allowlisted command request is pending.
    Inactive,
    /// One edit request may be validated and applied.
    AwaitingEdit,
    /// The pending command already submitted its edit request.
    EditSeen,
}

/// Kill a language-server process tree, then fall back to the direct child.
fn kill_server_process_tree(child: &mut Child)
{
    #[cfg(unix)]
    let _kill_result = rustix::process::kill_process_group(
        rustix::process::Pid::from_child(child),
        rustix::process::Signal::KILL,
    );
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        drop(
            Command::new("taskkill")
                .args(["/PID", &pid, "/T", "/F"])
                .status(),
        );
    }
    drop(child.kill());
}

/// Startup ownership that kills and reaps a spawned server until the complete
/// session takes responsibility for it.
struct StartingChild(Option<Child>);

impl Drop for StartingChild
{
    fn drop(&mut self)
    {
        if let Some(mut child) = self.0.take() {
            kill_server_process_tree(&mut child);
            drop(child.wait());
        }
    }
}

/// One direct-argv language-server process and its synchronized workspace.
struct LspSession
{
    /// Owned language-server child process.
    child: Option<Child>,
    /// Supervised JSON-RPC writer channel.
    writer: SyncSender<WriteRequest>,
    /// Reader-thread message channel.
    messages: Receiver<ReaderEvent>,
    /// Client responses received while a writer acknowledgement is pending.
    deferred_events: VecDeque<ReaderEvent>,
    /// Conservative decoded bytes retained by deferred events.
    deferred_event_bytes: usize,
    /// Bounded stderr suffix shared with the reader thread.
    stderr: Arc<Mutex<Vec<u8>>>,
    /// Canonical workspace root.
    root: Utf8PathBuf,
    /// RFC 8089 file URI for the workspace root.
    root_uri: String,
    /// Validated immutable session configuration.
    config: ResolvedCodeActionConfig,
    /// Absolute bound for the complete server session.
    session_deadline: Instant,
    /// Server-selected document synchronization strategy after initialization.
    text_sync: Option<TextDocumentSync>,
    /// Next JSON-RPC request identifier.
    next_id: u64,
    /// Request whose response may be deferred while servicing server traffic.
    pending_request_id: Option<u64>,
    /// Latest accepted diagnostic publication keyed by document URI.
    diagnostics: BTreeMap<String, Vec<Value>>,
    /// Latest diagnostics eligible to drive actions keyed by opened URI.
    actionable_diagnostics: BTreeMap<String, Vec<Value>>,
    /// Synchronized document version for each actionable publication.
    diagnostic_versions: BTreeMap<String, i64>,
    /// Highest versioned publication observed for each diagnostic URI.
    published_versions: BTreeMap<String, i64>,
    /// Estimated heap bytes retained by both diagnostic publication maps.
    diagnostic_bytes: usize,
    /// Generation incremented whenever accepted diagnostic state changes.
    diagnostic_generation: u64,
    /// Open documents awaiting an initial or refreshed publication.
    pending_diagnostics: BTreeSet<String>,
    /// Synchronized source documents keyed by file URI.
    documents: BTreeMap<String, OpenDocument>,
    /// Aggregate bytes retained by synchronized document snapshots.
    document_bytes: usize,
    /// Reentrancy guard while a workspace transaction is committing.
    applying_workspace_edit: bool,
    /// Edit-request scope for the currently pending allowlisted command.
    command_edit_scope: CommandEditScope,
    /// Whether unresolved action payloads may be resolved before selection.
    resolve_actions: bool,
    /// Number of diagnostic-correlated code-action requests issued.
    action_queries: usize,
    /// Count of successfully changed workspace transactions.
    mutation_count: u64,
    /// Reverse-ordered source history for whole-session rollback on failure.
    committed_edits: Vec<PreparedWorkspaceEdit>,
    /// Aggregate retained bytes in the whole-session rollback journal.
    committed_edit_bytes: usize,
    /// Total decoded server messages dispatched by this session.
    server_message_count: usize,
    /// Server requests nested through write backpressure.
    nested_server_requests: usize,
    /// Whether graceful shutdown has completed.
    stopped: bool,
}

impl LspSession
{
    /// Spawn and initialize one language server.
    fn start(
        config: &ResolvedCodeActionConfig,
        cwd: &Utf8Path,
    ) -> Result<Self, AifixError>
    {
        let session_deadline = Instant::now()
            .checked_add(config.timeout)
            .ok_or_else(|| AifixError::invalid_argument("LSP session timeout exceeded Instant"))?;
        let (executable, arguments) = config.argv.split_first().ok_or_else(|| {
            AifixError::invalid_argument("LSP code-action command requires an executable")
        })?;
        let root = canonical_utf8(cwd)?;
        let root_uri = file_uri(&root);
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut starting_child = StartingChild(Some(command.spawn().map_err(|error| {
            AifixError::process(format!(
                "failed to spawn LSP code-action server `{executable}`: {error}"
            ))
        })?));
        let child = starting_child
            .0
            .as_mut()
            .ok_or_else(|| AifixError::process("LSP startup child was unexpectedly absent"))?;
        let input = child.stdin.take().ok_or_else(|| {
            AifixError::process(format!(
                "LSP code-action server `{executable}` did not expose stdin"
            ))
        })?;
        let output = child.stdout.take().ok_or_else(|| {
            AifixError::process(format!(
                "LSP code-action server `{executable}` did not expose stdout"
            ))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AifixError::process(format!(
                "LSP code-action server `{executable}` did not expose stderr"
            ))
        })?;

        let (writer, write_requests) = mpsc::sync_channel(MAX_PENDING_LSP_WRITES);
        thread::Builder::new()
            .name("aifix-lsp-writer".to_owned())
            .spawn(move || write_messages(input, &write_requests))
            .map_err(|error| AifixError::process(format!("failed to spawn LSP writer: {error}")))?;
        let (sender, messages) = mpsc::sync_channel(MAX_PENDING_LSP_EVENTS);
        thread::Builder::new()
            .name("aifix-lsp-reader".to_owned())
            .spawn(move || read_messages(output, &sender))
            .map_err(|error| AifixError::process(format!("failed to spawn LSP reader: {error}")))?;
        let stderr_capture = Arc::new(Mutex::new(Vec::new()));
        let stderr_target = Arc::clone(&stderr_capture);
        thread::Builder::new()
            .name("aifix-lsp-stderr".to_owned())
            .spawn(move || drain_stderr(stderr, &stderr_target))
            .map_err(|error| {
                AifixError::process(format!("failed to spawn LSP stderr reader: {error}"))
            })?;
        let owned_child = starting_child
            .0
            .take()
            .ok_or_else(|| AifixError::process("LSP startup child was unexpectedly absent"))?;

        let session = Self {
            child: Some(owned_child),
            writer,
            messages,
            stderr: stderr_capture,
            deferred_events: VecDeque::new(),
            deferred_event_bytes: 0,
            root,
            root_uri,
            config: config.clone(),
            session_deadline,
            text_sync: None,
            next_id: 1,
            pending_request_id: None,
            diagnostics: BTreeMap::new(),
            actionable_diagnostics: BTreeMap::new(),
            diagnostic_versions: BTreeMap::new(),
            published_versions: BTreeMap::new(),
            diagnostic_generation: 0,
            pending_diagnostics: BTreeSet::new(),
            diagnostic_bytes: 0,
            documents: BTreeMap::new(),
            resolve_actions: false,
            document_bytes: 0,
            action_queries: 0,
            mutation_count: 0,
            committed_edits: Vec::new(),
            committed_edit_bytes: 0,
            server_message_count: 0,
            nested_server_requests: 0,
            stopped: false,
            applying_workspace_edit: false,
            command_edit_scope: CommandEditScope::Inactive,
        };
        Ok(session)
    }

    /// Initialize capabilities, open matching source files, and collect the
    /// first stable diagnostic publication set.
    fn initialize(&mut self) -> Result<(), AifixError>
    {
        self.negotiate()?;
        let sources =
            discover_source_files(&self.root, &self.config.extensions, self.session_deadline)?;
        for path in sources {
            self.open_document(path)?;
            self.drain_server_events()?;
        }
        self.wait_for_diagnostics()
    }

    /// Negotiate required server capabilities without opening source files.
    fn negotiate(&mut self) -> Result<(), AifixError>
    {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "aifix", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": self.root_uri,
            "workspaceFolders": [{ "uri": self.root_uri, "name": "workspace" }],
            "capabilities": {
                "workspace": {
                    "applyEdit": !self.config.allowed_commands.is_empty(),
                    "workspaceEdit": {
                        "documentChanges": true,
                        "resourceOperations": Vec::<String>::new(),
                        "failureHandling": "abort"
                    },
                    "configuration": true,
                    "workspaceFolders": true
                },
                "textDocument": {
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": true,
                        "codeDescriptionSupport": true,
                        "dataSupport": true
                    },
                    "codeAction": {
                        "dynamicRegistration": false,
                        "codeActionLiteralSupport": {
                            "codeActionKind": { "valueSet": self.config.action_kinds }
                        },
                        "resolveSupport": { "properties": ["edit", "command"] },
                        "dataSupport": true,
                        "isPreferredSupport": true,
                        "disabledSupport": true,
                    },
                    "synchronization": {
                        "dynamicRegistration": false,
                        "didSave": false
                    }
                },
                "window": { "workDoneProgress": true }
            }
        });
        let result = self.request("initialize", &params)?;
        self.text_sync = Some(resolve_text_document_sync(&result)?);
        self.resolve_actions = resolve_code_action_provider(&result)?;
        validate_execute_command_provider(&result, &self.config.allowed_commands)?;
        self.notify("initialized", &json!({}))
    }

    /// Apply one safe action at a time until no applicable action remains.
    fn run(&mut self) -> Result<Vec<Diagnostic>, AifixError>
    {
        let mut applied = BTreeSet::new();
        let mut applied_bytes = 0_usize;
        for _ in 0 .. self.config.max_iterations {
            let Some(action) = self.next_safe_action(&applied)?
            else {
                return self.normalized_diagnostics();
            };
            let key = action.key.clone();
            let key_bytes = key.retained_bytes()?;
            let projected_applied_bytes = applied_bytes
                .checked_add(key_bytes)
                .ok_or_else(|| AifixError::process("LSP action-key byte accounting overflowed"))?;
            if projected_applied_bytes > MAX_RETAINED_ACTION_KEY_BYTES {
                return Err(AifixError::process(format!(
                    "LSP session exceeded {MAX_RETAINED_ACTION_KEY_BYTES} retained action-key bytes"
                )));
            }
            let before = self.mutation_count;
            self.apply_action(&action.value)?;
            if self.mutation_count == before {
                return Err(AifixError::process(format!(
                    "LSP code action `{}` reported success without changing the workspace",
                    key.title
                )));
            }
            applied.insert(key);
            applied_bytes = projected_applied_bytes;
            self.wait_for_diagnostics()?;
        }
        if self.next_safe_action(&applied)?.is_some() {
            return Err(AifixError::process(format!(
                "LSP code actions did not converge within {} iterations",
                self.config.max_iterations
            )));
        }
        self.normalized_diagnostics()
    }

    /// Return the first deterministic, unambiguous safe action.
    fn next_safe_action(
        &mut self,
        applied: &BTreeSet<ActionKey>,
    ) -> Result<Option<SelectedAction>, AifixError>
    {
        'publication: loop {
            let generation = self.diagnostic_generation;
            let diagnostics = self.actionable_diagnostics.clone();
            for (uri, values) in diagnostics {
                let Some(document) = self.documents.get(&uri)
                else {
                    continue;
                };
                if self.diagnostic_versions.get(&uri).copied() != Some(document.version) {
                    continue;
                }
                for diagnostic in values {
                    let Some(mut candidates) = self.code_actions(&uri, &diagnostic, generation)?
                    else {
                        continue 'publication;
                    };
                    let repeated = candidates
                        .iter()
                        .any(|candidate| applied.contains(&candidate.key));
                    candidates.retain(|candidate| !applied.contains(&candidate.key));
                    if candidates.is_empty() && repeated {
                        return Err(AifixError::process(
                            "LSP code action repeated for an unchanged diagnostic",
                        ));
                    }
                    let preferred = candidates
                        .iter()
                        .filter(|candidate| candidate.preferred)
                        .cloned()
                        .collect::<Vec<_>>();
                    if preferred.len() == 1 {
                        return Ok(preferred.into_iter().next());
                    }
                    if preferred.is_empty() && candidates.len() == 1 {
                        return Ok(candidates.into_iter().next());
                    }
                }
            }
            if generation != self.diagnostic_generation {
                continue;
            }
            return Ok(None);
        }
    }

    /// Request and filter code actions for one published diagnostic.
    fn code_actions(
        &mut self,
        uri: &str,
        diagnostic: &Value,
        generation: u64,
    ) -> Result<Option<Vec<SelectedAction>>, AifixError>
    {
        let range = diagnostic.get("range").cloned().ok_or_else(|| {
            AifixError::process("LSP diagnostic eligible for code actions had no range")
        })?;
        if self.action_queries >= MAX_ACTION_QUERIES {
            return Err(AifixError::process(format!(
                "LSP code-action session exceeded {MAX_ACTION_QUERIES} diagnostic queries"
            )));
        }
        self.action_queries = self
            .action_queries
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP action-query counter overflowed"))?;
        let result = self.request(
            "textDocument/codeAction",
            &json!({
                "textDocument": { "uri": uri },
                "range": range,
                "context": {
                    "diagnostics": [diagnostic],
                    "only": self.config.action_kinds,
                    "triggerKind": 1_u64
                }
            }),
        )?;
        if generation != self.diagnostic_generation {
            return Ok(None);
        }
        let Some(actions) = result.as_array()
        else {
            if result.is_null() {
                return Ok(Some(Vec::new()));
            }
            return Err(AifixError::process(
                "LSP textDocument/codeAction response was not an array or null",
            ));
        };

        if actions.len() > MAX_ACTIONS_PER_RESPONSE {
            return Err(AifixError::process(format!(
                "LSP code-action response exceeded {MAX_ACTIONS_PER_RESPONSE} actions"
            )));
        }
        let diagnostic_key = Arc::new(DiagnosticKey::from_diagnostic(diagnostic));
        let mut selected_bytes = diagnostic_key.retained_bytes()?;
        let mut selected = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            if index % 256 == 0 {
                ensure_deadline(self.session_deadline, "LSP code-action selection")?;
            }
            if !action.is_object() || action.get("title").and_then(Value::as_str).is_none() {
                return Err(AifixError::process(
                    "LSP code-action response contained an action without an object title",
                ));
            }
            let preferred = action_preferred(action)?;
            let mut action = action.clone();
            if action.get("disabled").is_some() || !self.action_kind_allowed(&action) {
                continue;
            }
            if !action_matches_diagnostic(&action, &diagnostic_key)? {
                continue;
            }
            if self.resolve_actions && action.get("edit").is_none() && action.get("data").is_some()
            {
                let unresolved = action.clone();
                action = self.request("codeAction/resolve", &action)?;
                if generation != self.diagnostic_generation {
                    return Ok(None);
                }
                let _ = action_preferred(&action)?;
                if !resolved_metadata_preserved(&unresolved, &action) {
                    continue;
                }
                if action.get("disabled").is_some() || !self.action_kind_allowed(&action) {
                    continue;
                }
                if !action_matches_diagnostic(&action, &diagnostic_key)? {
                    continue;
                }
            }
            if !self.action_payload_allowed(&action) {
                continue;
            }
            if let Some(edit) = action.get("edit")
                && !self.workspace_edit_applicable(edit)?
            {
                continue;
            }
            let key = ActionKey::from_action(uri, &diagnostic_key, &action);
            selected_bytes = selected_bytes
                .checked_add(key.retained_bytes_without_diagnostic()?)
                .ok_or_else(|| {
                    AifixError::process("LSP action-candidate byte accounting overflowed")
                })?;
            if selected_bytes > MAX_ACTION_CANDIDATE_BYTES {
                return Err(AifixError::process(format!(
                    "LSP code-action response exceeded {MAX_ACTION_CANDIDATE_BYTES} retained candidate bytes"
                )));
            }
            selected.push(SelectedAction {
                preferred,
                key,
                value: action,
            });
        }
        selected.sort_by(|left, right| left.key.cmp(&right.key));
        selected.dedup_by(|left, right| left.key == right.key && left.value == right.value);
        Ok(Some(selected))
    }

    /// Return whether an action's hierarchical kind is allowlisted.
    fn action_kind_allowed(
        &self,
        action: &Value,
    ) -> bool
    {
        let Some(kind) = action.get("kind").and_then(Value::as_str)
        else {
            return action.get("command").is_some_and(Value::is_string)
                && self.action_command_allowed(action);
        };
        self.config.action_kinds.iter().any(|allowed| {
            kind == allowed
                || kind
                    .strip_prefix(allowed)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    }

    /// Return whether an action contains one direct workspace edit and no
    /// command payload.
    fn action_payload_allowed(
        &self,
        action: &Value,
    ) -> bool
    {
        let has_edit = action.get("edit").is_some_and(|edit| !edit.is_null());
        let has_command_field = action.get("command").is_some();
        has_edit ^ has_command_field && (!has_command_field || self.action_command_allowed(action))
    }

    /// Return whether an action carries an explicitly allowlisted command.
    fn action_command_allowed(
        &self,
        action: &Value,
    ) -> bool
    {
        command_identifier(action).is_some_and(|command| {
            self.config
                .allowed_commands
                .binary_search_by(|allowed| allowed.as_str().cmp(command))
                .is_ok()
        })
    }

    /// Prevalidate one direct workspace edit without mutating source.
    ///
    /// Automatic-safety policy rejections make a candidate ineligible;
    /// malformed protocol shapes remain typed failures.
    fn workspace_edit_applicable(
        &self,
        edit: &Value,
    ) -> Result<bool, AifixError>
    {
        let validate = || -> Result<bool, AifixError> {
            self.validate_workspace_edit_versions(edit)?;
            let edits = collect_workspace_edits(edit)?;
            let mut changes_content = false;
            for (uri, text_edits) in edits {
                let path = self.workspace_path(&uri)?;
                let opened = self.documents.get(&uri).ok_or_else(|| {
                    AifixError::invalid_argument(format!(
                        "LSP workspace edit targeted unopened document `{uri}`"
                    ))
                })?;
                if opened.path != path {
                    return Err(AifixError::invalid_argument(format!(
                        "LSP workspace edit target changed after the document was opened: `{uri}`"
                    )));
                }
                let metadata = fs::metadata(&path)
                    .map_err(|error| AifixError::io_path(path.clone(), error))?;
                validate_replacement_metadata(&path, &metadata)?;
                if !file_matches_expected(&path, opened.text.as_bytes())? {
                    return Ok(false);
                }
                let updated = apply_text_edits(&opened.text, &text_edits, self.session_deadline)?;
                changes_content |= updated != opened.text;
            }
            Ok(changes_content)
        };
        match validate() {
            | Err(AifixError::InvalidArgument(_)) => Ok(false),
            | result => result,
        }
    }

    /// Apply one selected direct workspace edit or scoped command.
    fn apply_action(
        &mut self,
        action: &Value,
    ) -> Result<(), AifixError>
    {
        if let Some(edit) = action.get("edit") {
            self.apply_workspace_edit(edit)?;
            return Ok(());
        }
        let command = command_payload(action).ok_or_else(|| {
            AifixError::process("eligible LSP code action had neither an edit nor a command")
        })?;
        self.command_edit_scope = CommandEditScope::AwaitingEdit;
        let result = self.request("workspace/executeCommand", command);
        self.command_edit_scope = CommandEditScope::Inactive;
        result.map(drop)
    }

    /// Validate optional versioned document edits against synchronized state.
    fn validate_workspace_edit_versions(
        &self,
        edit: &Value,
    ) -> Result<(), AifixError>
    {
        let Some(changes) = edit.get("documentChanges")
        else {
            return Ok(());
        };
        let changes = changes.as_array().ok_or_else(|| {
            AifixError::process("LSP WorkspaceEdit.documentChanges was not an array")
        })?;
        for change in changes {
            let Some(document) = change.get("textDocument")
            else {
                continue;
            };
            let uri = document
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| AifixError::process("LSP TextDocumentEdit had no document URI"))?;
            let opened = self.documents.get(uri).ok_or_else(|| {
                AifixError::invalid_argument(format!(
                    "LSP workspace edit targeted unopened document `{uri}`"
                ))
            })?;
            let version = document.get("version").ok_or_else(|| {
                AifixError::process("LSP TextDocumentEdit had no document version")
            })?;
            if version.is_null() {
                continue;
            }
            let version = version.as_i64().ok_or_else(|| {
                AifixError::process("LSP TextDocumentEdit version was not an integer or null")
            })?;
            if version != opened.version {
                return Err(AifixError::process(format!(
                    "LSP TextDocumentEdit version {version} did not match open version {}",
                    opened.version
                )));
            }
        }
        Ok(())
    }

    /// Apply a workspace edit with an explicit reentrancy guard.
    fn apply_workspace_edit(
        &mut self,
        edit: &Value,
    ) -> Result<bool, AifixError>
    {
        if self.applying_workspace_edit {
            return Err(AifixError::invalid_argument(
                "nested LSP workspace edits are not automatic-safe",
            ));
        }
        self.applying_workspace_edit = true;
        let result = self.apply_workspace_edit_inner(edit);
        self.applying_workspace_edit = false;
        result
    }

    /// Apply a validated, staged workspace text-edit transaction.
    fn apply_workspace_edit_inner(
        &mut self,
        edit: &Value,
    ) -> Result<bool, AifixError>
    {
        self.validate_workspace_edit_versions(edit)?;
        let edits = collect_workspace_edits(edit)?;
        let mut prepared = Vec::with_capacity(edits.len());
        let mut projected_document_bytes = self.document_bytes;
        let mut staged_bytes = 0_usize;
        for (uri, text_edits) in edits {
            let path = self.workspace_path(&uri)?;
            let opened = self.documents.get(&uri).ok_or_else(|| {
                AifixError::invalid_argument(format!(
                    "LSP workspace edit targeted unopened document `{uri}`"
                ))
            })?;
            if opened.path != path {
                return Err(AifixError::invalid_argument(format!(
                    "LSP workspace edit target changed after the document was opened: `{uri}`"
                )));
            }
            if !file_matches_expected(&path, opened.text.as_bytes())? {
                return Err(AifixError::invalid_argument(format!(
                    "LSP workspace edit target changed after synchronization: `{uri}`"
                )));
            }
            let updated = apply_text_edits(&opened.text, &text_edits, self.session_deadline)?;
            if updated == opened.text {
                continue;
            }
            let previous_document_bytes =
                document_entry_retained_bytes(&uri, &opened.path, opened.text.len())?;
            let updated_document_bytes = document_entry_retained_bytes(&uri, &path, updated.len())?;
            projected_document_bytes = replace_retained_bytes(
                projected_document_bytes,
                previous_document_bytes,
                updated_document_bytes,
                "LSP document",
            )?;
            if projected_document_bytes > MAX_OPEN_DOCUMENT_BYTES {
                return Err(AifixError::process(format!(
                    "LSP session exceeded {MAX_OPEN_DOCUMENT_BYTES} retained document bytes"
                )));
            }
            self.prepare_document_change(&uri, opened, &updated)?;
            staged_bytes = staged_bytes
                .checked_add(
                    opened
                        .text
                        .len()
                        .checked_add(updated.len())
                        .and_then(|bytes| bytes.checked_add(512))
                        .ok_or_else(|| {
                            AifixError::process("LSP staged edit byte accounting overflowed")
                        })?,
                )
                .ok_or_else(|| AifixError::process("LSP staged edit byte accounting overflowed"))?;
            if staged_bytes > MAX_STAGED_WORKSPACE_EDIT_BYTES {
                return Err(AifixError::process(format!(
                    "LSP workspace transaction exceeded {MAX_STAGED_WORKSPACE_EDIT_BYTES} staged bytes"
                )));
            }
            prepared.push(PreparedWorkspaceEdit {
                uri,
                path,
                expected: opened.text.clone(),
                updated,
                temporary: None,
            });
        }
        if prepared.is_empty() {
            return Ok(false);
        }
        let committed_edit_bytes = self
            .committed_edit_bytes
            .checked_add(staged_bytes)
            .ok_or_else(|| {
                AifixError::process("LSP rollback journal byte accounting overflowed")
            })?;
        if committed_edit_bytes > MAX_STAGED_WORKSPACE_EDIT_BYTES {
            return Err(AifixError::process(format!(
                "LSP session exceeded {MAX_STAGED_WORKSPACE_EDIT_BYTES} rollback journal bytes"
            )));
        }
        let mut stage_failure = None;
        for change in &mut prepared {
            match stage_atomic_replacement(&change.path, &change.expected, &change.updated) {
                | Ok(temporary) => change.temporary = Some(temporary),
                | Err(error) => {
                    stage_failure = Some(error);
                    break;
                },
            }
        }
        if let Some(error) = stage_failure {
            return Err(with_staged_cleanup(error, &mut prepared));
        }
        let mut committed = 0_usize;
        while committed < prepared.len() {
            let result = {
                let Some(change) = prepared.get_mut(committed)
                else {
                    return Err(with_staged_cleanup(
                        AifixError::process("staged LSP workspace edit accounting failed"),
                        &mut prepared,
                    ));
                };
                let Some(temporary) = change.temporary.take()
                else {
                    return Err(with_staged_cleanup(
                        AifixError::process("staged LSP edit was unexpectedly absent"),
                        &mut prepared,
                    ));
                };
                replace_staged_if_unchanged(&change.path, &change.expected, &temporary)
            };
            if let Err(error) = result {
                let error = with_staged_cleanup(error, &mut prepared);
                let Some(applied) = prepared.get(.. committed)
                else {
                    return Err(AifixError::process(
                        "committed LSP workspace edit accounting failed",
                    ));
                };
                return Err(with_workspace_rollback(error, applied));
            }
            committed = committed.saturating_add(1);
        }
        for change in &prepared {
            if let Err(error) =
                self.synchronize_document(&change.uri, change.path.clone(), change.updated.clone())
            {
                return Err(with_workspace_rollback(error, &prepared));
            }
            if let Err(error) = self.drain_server_events() {
                return Err(with_workspace_rollback(error, &prepared));
            }
        }
        self.committed_edits.extend(prepared);
        self.committed_edit_bytes = committed_edit_bytes;
        self.mutation_count = self
            .mutation_count
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP mutation counter overflowed"))?;
        Ok(true)
    }

    /// Update an opened document using the server-selected synchronization
    /// mode.
    fn synchronize_document(
        &mut self,
        uri: &str,
        path: Utf8PathBuf,
        text: String,
    ) -> Result<(), AifixError>
    {
        let document = self.documents.get(uri).ok_or_else(|| {
            AifixError::process(format!("cannot synchronize unopened LSP document `{uri}`"))
        })?;
        let previous_document_bytes =
            document_entry_retained_bytes(uri, &document.path, document.text.len())?;
        let updated_document_bytes = document_entry_retained_bytes(uri, &path, text.len())?;
        let retained_document_bytes = replace_retained_bytes(
            self.document_bytes,
            previous_document_bytes,
            updated_document_bytes,
            "LSP document",
        )?;
        if retained_document_bytes > MAX_OPEN_DOCUMENT_BYTES {
            return Err(AifixError::process(format!(
                "LSP session exceeded {MAX_OPEN_DOCUMENT_BYTES} retained document bytes"
            )));
        }
        let (version, params) = self.prepare_document_change(uri, document, &text)?;
        self.clear_document_diagnostics(uri)?;
        self.bump_diagnostic_generation()?;
        self.pending_diagnostics.insert(uri.to_owned());
        self.documents.insert(uri.to_owned(), OpenDocument {
            path,
            text,
            version,
        });
        self.document_bytes = retained_document_bytes;
        self.notify("textDocument/didChange", &params)?;
        Ok(())
    }

    /// Build and size-check the exact notification for one document change.
    fn prepare_document_change(
        &self,
        uri: &str,
        document: &OpenDocument,
        text: &str,
    ) -> Result<(i64, Value), AifixError>
    {
        let version = document
            .version
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP document version overflowed"))?;
        let change = match self.text_sync.ok_or_else(|| {
            AifixError::process("LSP server document synchronization was not initialized")
        })? {
            | TextDocumentSync::Full => json!({ "text": text }),
            | TextDocumentSync::Incremental => {
                let (line, character) =
                    document_end_position(&document.text, self.session_deadline)?;
                json!({
                    "range": {
                        "start": { "line": 0_u64, "character": 0_u64 },
                        "end": { "line": line, "character": character }
                    },
                    "text": text
                })
            },
        };
        let params = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [change]
        });
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": &params
        });
        let payload_bytes = serde_json::to_vec(&notification)
            .map_err(|error| {
                AifixError::process(format!("failed to serialize LSP message: {error}"))
            })?
            .len();
        if payload_bytes > MAX_LSP_MESSAGE_BYTES {
            return Err(AifixError::process(format!(
                "outgoing LSP message exceeded {MAX_LSP_MESSAGE_BYTES} bytes"
            )));
        }
        Ok((version, params))
    }

    /// Clear stale diagnostic state after a synchronized local mutation.
    fn clear_document_diagnostics(
        &mut self,
        uri: &str,
    ) -> Result<(), AifixError>
    {
        let residual_bytes = self
            .diagnostics
            .get(uri)
            .map_or(Ok(0), |values| diagnostic_entry_retained_bytes(uri, values))?;
        let actionable_bytes = self
            .actionable_diagnostics
            .get(uri)
            .map_or(Ok(0), |values| actionable_entry_retained_bytes(uri, values))?;
        let removed_bytes = residual_bytes
            .checked_add(actionable_bytes)
            .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
        self.diagnostic_bytes =
            replace_retained_bytes(self.diagnostic_bytes, removed_bytes, 0, "LSP diagnostic")?;
        self.diagnostics.remove(uri);
        self.actionable_diagnostics.remove(uri);
        self.diagnostic_versions.remove(uri);
        Ok(())
    }

    /// Open one source document with full text synchronization.
    fn open_document(
        &mut self,
        path: Utf8PathBuf,
    ) -> Result<(), AifixError>
    {
        let uri = file_uri(&path);
        if self.documents.contains_key(&uri) {
            return Err(AifixError::process(format!(
                "LSP source document was opened more than once: `{uri}`"
            )));
        }
        let fixed_bytes = document_entry_retained_bytes(&uri, &path, 0)?;
        let base_bytes = self
            .document_bytes
            .checked_add(fixed_bytes)
            .ok_or_else(|| AifixError::process("LSP document byte accounting overflowed"))?;
        let available = MAX_OPEN_DOCUMENT_BYTES
            .checked_sub(base_bytes)
            .ok_or_else(|| {
                AifixError::process(format!(
                    "LSP session exceeded {MAX_OPEN_DOCUMENT_BYTES} retained document bytes"
                ))
            })?;
        let text = read_utf8_file_bounded(
            &path,
            available.min(MAX_LSP_MESSAGE_BYTES),
            "LSP source document",
        )?;
        let retained_document_bytes = base_bytes
            .checked_add(text.len())
            .ok_or_else(|| AifixError::process("LSP document byte accounting overflowed"))?;
        let previous_document_bytes = self.document_bytes;
        self.pending_diagnostics.insert(uri.clone());
        self.documents.insert(uri.clone(), OpenDocument {
            path,
            text: text.clone(),
            version: 0,
        });
        self.document_bytes = retained_document_bytes;
        if let Err(error) = self.notify(
            "textDocument/didOpen",
            &json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": self.config.language_id,
                    "version": 0_u64,
                    "text": text
                }
            }),
        ) {
            self.pending_diagnostics.remove(&uri);
            self.documents.remove(&uri);
            self.document_bytes = previous_document_bytes;
            return Err(error);
        }
        Ok(())
    }

    /// Wait until LSP traffic is quiet for one bounded idle interval.
    ///
    /// A server need not publish an empty diagnostic set for a newly opened
    /// clean document, so publication from every pending document cannot be a
    /// completion requirement. Each received message restarts the idle wait.
    fn wait_for_diagnostics(&mut self) -> Result<(), AifixError>
    {
        let deadline = self.operation_deadline()?;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(AifixError::process(format!(
                    "LSP diagnostic refresh timed out after {} ms with {} documents still pending{}",
                    self.config.timeout.as_millis(),
                    self.pending_diagnostics.len(),
                    self.stderr_suffix()
                )));
            }
            let remaining = deadline.saturating_duration_since(now);
            let wait = remaining.min(Duration::from_millis(DIAGNOSTIC_IDLE_MS));
            let received = self
                .pop_deferred_event()?
                .map_or_else(|| self.messages.recv_timeout(wait), Ok);
            match received {
                | Ok(event) => {
                    self.handle_event(&event, None)?;
                },
                | Err(RecvTimeoutError::Timeout) => return Ok(()),
                | Err(RecvTimeoutError::Disconnected) => return Err(self.disconnected_error()),
            }
        }
    }

    /// Service all currently queued server traffic between sequential
    /// document notifications so bounded channels cannot stall the server.
    fn drain_server_events(&mut self) -> Result<(), AifixError>
    {
        loop {
            if Instant::now() >= self.session_deadline {
                return Err(AifixError::process(format!(
                    "LSP server event drain exceeded the session deadline{}",
                    self.stderr_suffix()
                )));
            }
            if self.pending_request_id.is_some()
                && self
                    .deferred_events
                    .front()
                    .is_some_and(Self::reader_event_is_client_response)
            {
                return Ok(());
            }
            let event = if let Some(event) = self.pop_deferred_event()? {
                Some(event)
            }
            else {
                match self.messages.try_recv() {
                    | Ok(event) => Some(event),
                    | Err(TryRecvError::Empty) => None,
                    | Err(TryRecvError::Disconnected) => return Err(self.disconnected_error()),
                }
            };
            let Some(event) = event
            else {
                return Ok(());
            };
            if self.pending_request_id.is_some() && Self::reader_event_is_client_response(&event) {
                self.defer_event(event)?;
                return Ok(());
            }
            self.handle_event(&event, None)?;
        }
    }

    /// Convert the current publication map through the existing LSP adapter.
    fn normalized_diagnostics(&self) -> Result<Vec<Diagnostic>, AifixError>
    {
        let mut normalized = Vec::new();
        for (uri, diagnostics) in &self.diagnostics {
            let value = json!({ "uri": uri, "diagnostics": diagnostics });
            normalized.extend(parse_lsp_value(&value)?);
        }
        Ok(normalized)
    }
    /// Return whether an event is a client-request response that must remain
    /// queued for the active request loop.
    fn reader_event_is_client_response(event: &ReaderEvent) -> bool
    {
        event
            .as_ref()
            .is_ok_and(|message| message.get("method").is_none() && message.get("id").is_some())
    }

    /// Resolve one file URI and enforce workspace containment.
    fn workspace_path(
        &self,
        uri: &str,
    ) -> Result<Utf8PathBuf, AifixError>
    {
        let path = path_from_file_uri(uri)?;
        let canonical = canonical_utf8(&path)?;
        if !canonical.starts_with(&self.root) {
            return Err(AifixError::invalid_argument(format!(
                "LSP workspace edit targeted path outside the configured root: {canonical}"
            )));
        }
        Ok(canonical)
    }

    /// Send one request and wait while servicing server traffic.
    fn request(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, AifixError>
    {
        let deadline = self.operation_deadline()?;
        let mut content_modified_retries = 0;
        'request: loop {
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
                AifixError::process("LSP request identifier overflowed its bounded counter")
            })?;
            self.pending_request_id = Some(id);
            self.send(
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params
                }),
                deadline,
            )?;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(AifixError::process(format!(
                        "LSP request `{method}` timed out after {} ms{}",
                        self.config.timeout.as_millis(),
                        self.stderr_suffix()
                    )));
                }
                let received = self
                    .pop_deferred_event()?
                    .map_or_else(|| self.messages.recv_timeout(remaining), Ok);
                match received {
                    | Ok(event) => {
                        let outcome = self.handle_event(&event, Some(id))?;
                        if outcome.is_some() {
                            self.pending_request_id = None;
                        }
                        match outcome {
                            | Some(ResponseOutcome::Result(result)) => return Ok(result),
                            | Some(ResponseOutcome::ContentModified)
                                if content_modified_retries < MAX_CONTENT_MODIFIED_RETRIES =>
                            {
                                content_modified_retries += 1;
                                continue 'request;
                            },
                            | Some(ResponseOutcome::ContentModified) => {
                                return Err(AifixError::process(format!(
                                    "LSP request `{method}` remained content-modified after \
                                     {MAX_CONTENT_MODIFIED_RETRIES} retries{}",
                                    self.stderr_suffix()
                                )));
                            },
                            | None => {},
                        }
                    },
                    | Err(RecvTimeoutError::Timeout) => {
                        return Err(AifixError::process(format!(
                            "LSP request `{method}` timed out after {} ms{}",
                            self.config.timeout.as_millis(),
                            self.stderr_suffix()
                        )));
                    },
                    | Err(RecvTimeoutError::Disconnected) => {
                        return Err(self.disconnected_error());
                    },
                }
            }
        }
    }

    /// Handle one reader event and optionally return a matching response.
    fn handle_event(
        &mut self,
        event: &ReaderEvent,
        expected_id: Option<u64>,
    ) -> Result<Option<ResponseOutcome>, AifixError>
    {
        self.server_message_count = self
            .server_message_count
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP server message counter overflowed"))?;
        if self.server_message_count > MAX_SERVER_MESSAGES {
            return Err(AifixError::process(format!(
                "LSP session exceeded {MAX_SERVER_MESSAGES} server messages"
            )));
        }
        let message = event.as_ref().map_err(|error| {
            AifixError::process(format!(
                "invalid LSP server message: {error}{}",
                self.stderr_suffix()
            ))
        })?;
        let object = message
            .as_object()
            .ok_or_else(|| AifixError::process("LSP server message was not a JSON object"))?;
        let version = object
            .get("jsonrpc")
            .ok_or_else(|| AifixError::process("LSP server message had no jsonrpc version"))?;
        if version.as_str() != Some("2.0") {
            return Err(AifixError::process(
                "LSP server message jsonrpc field was not \"2.0\"",
            ));
        }
        if let Some(method) = object.get("method") {
            if method.as_str().is_none() {
                return Err(AifixError::process(
                    "LSP server message method was not a string",
                ));
            }
            if object.get("params").is_some_and(|params| {
                !params.is_null() && !params.is_object() && !params.is_array()
            }) {
                return Err(AifixError::process(
                    "LSP server message params were not an object, array, or null",
                ));
            }
            if let Some(id) = object.get("id") {
                if !json_rpc_request_id_is_valid(id) {
                    return Err(AifixError::process(
                        "LSP server request id was not a string or integer",
                    ));
                }
                if self.nested_server_requests >= MAX_NESTED_SERVER_REQUESTS {
                    return Err(AifixError::process(format!(
                        "LSP session exceeded {MAX_NESTED_SERVER_REQUESTS} nested server requests"
                    )));
                }
                self.nested_server_requests += 1;
                let result = self.handle_server_request(message);
                self.nested_server_requests -= 1;
                result?;
            }
            else {
                self.handle_notification(message)?;
            }
            return Ok(None);
        }
        if let Some(response_id) = message.get("id") {
            let response_id = response_id.as_u64().ok_or_else(|| {
                AifixError::process("LSP response id was not an unsigned integer")
            })?;
            if expected_id != Some(response_id) {
                return Err(AifixError::process(format!(
                    "LSP response id {response_id} did not match the pending request"
                )));
            }
            return match (message.get("result"), message.get("error")) {
                | (Some(result), None) => Ok(Some(ResponseOutcome::Result(result.clone()))),
                | (None, Some(error)) => {
                    let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| {
                        AifixError::process("LSP response error had no integer code")
                    })?;
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            AifixError::process("LSP response error had no string message")
                        })?;
                    if code == -32801 {
                        Ok(Some(ResponseOutcome::ContentModified))
                    }
                    else {
                        Err(AifixError::process(format!(
                            "LSP request failed: {error}{}",
                            self.stderr_suffix()
                        )))
                    }
                },
                | _ => Err(AifixError::process(
                    "LSP response must contain exactly one of result or error",
                )),
            };
        }
        Err(AifixError::process(
            "LSP server message contained neither a method nor a response id",
        ))
    }

    /// Handle one server-to-client request.
    fn handle_server_request(
        &mut self,
        message: &Value,
    ) -> Result<(), AifixError>
    {
        let id = message
            .get("id")
            .cloned()
            .ok_or_else(|| AifixError::process("LSP server request had no id"))?;
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| AifixError::process("LSP server request had no method"))?;
        match method {
            | "workspace/applyEdit" => {
                if self.command_edit_scope != CommandEditScope::AwaitingEdit {
                    return self.respond(
                        &id,
                        &json!({
                            "applied": false,
                            "failureReason": "server edit was outside one allowlisted command scope"
                        }),
                    );
                }
                self.command_edit_scope = CommandEditScope::EditSeen;
                let Some(edit) = message.get("params").and_then(|params| params.get("edit"))
                else {
                    return self.respond(
                        &id,
                        &json!({
                            "applied": false,
                            "failureReason": "workspace/applyEdit params omitted the edit"
                        }),
                    );
                };
                let applicable = self.workspace_edit_applicable(edit).unwrap_or(false);
                if !applicable {
                    return self.respond(
                        &id,
                        &json!({
                            "applied": false,
                            "failureReason": "workspace edit was not automatically applicable"
                        }),
                    );
                }
                self.apply_workspace_edit(edit)?;
                self.respond(&id, &json!({ "applied": true }))
            },
            | "workspace/configuration" => {
                let count = message
                    .get("params")
                    .and_then(|params| params.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                self.respond(&id, &Value::Array(vec![Value::Null; count]))
            },
            | "workspace/workspaceFolders" => {
                self.respond(&id, &json!([{ "uri": self.root_uri, "name": "workspace" }]))
            },
            | "window/workDoneProgress/create" => self.respond(&id, &Value::Null),
            | _ => {
                let deadline = self.operation_deadline()?;
                self.send(
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": "method not supported by aifix" }
                    }),
                    deadline,
                )
            },
        }
    }

    /// Record one server notification relevant to the session.
    fn handle_notification(
        &mut self,
        message: &Value,
    ) -> Result<(), AifixError>
    {
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        {
            let params = message.get("params").ok_or_else(|| {
                AifixError::process("LSP publishDiagnostics notification had no params")
            })?;
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .filter(|uri| !uri.is_empty())
                .ok_or_else(|| {
                    AifixError::process("LSP publishDiagnostics notification had no URI")
                })?;
            let opened_version = self.documents.get(uri).map(|document| document.version);
            let published_version = match params.get("version") {
                | None | Some(&Value::Null) => None,
                | Some(version) => Some(version.as_i64().ok_or_else(|| {
                    AifixError::process("LSP diagnostic version was not an integer or null")
                })?),
            };
            if let (Some(opened), Some(published)) = (opened_version, published_version) {
                if published > opened {
                    return Err(AifixError::process(format!(
                        "LSP diagnostic version exceeded open version for `{uri}`"
                    )));
                }
                if published < opened {
                    return Ok(());
                }
            }
            if matches!(
                (self.published_versions.get(uri).copied(), published_version),
                (Some(previous), Some(published)) if published < previous
            ) {
                return Ok(());
            }
            let diagnostics = params
                .get("diagnostics")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    AifixError::process("LSP publishDiagnostics payload was not an array")
                })?;
            drop(parse_lsp_value(params)?);
            if !self.diagnostics.contains_key(uri)
                && self.diagnostics.len() >= MAX_DIAGNOSTIC_DOCUMENTS
            {
                return Err(AifixError::process(format!(
                    "LSP session exceeded {MAX_DIAGNOSTIC_DOCUMENTS} diagnostic documents"
                )));
            }
            let previous_count = self.diagnostics.get(uri).map_or(0, Vec::len);
            let retained_count = self
                .diagnostics
                .values()
                .try_fold(0_usize, |total, values| total.checked_add(values.len()))
                .and_then(|total| total.checked_sub(previous_count))
                .and_then(|total| total.checked_add(diagnostics.len()))
                .ok_or_else(|| AifixError::process("LSP diagnostic count overflowed"))?;
            if retained_count > MAX_PUBLISHED_DIAGNOSTICS {
                return Err(AifixError::process(format!(
                    "LSP session exceeded {MAX_PUBLISHED_DIAGNOSTICS} published diagnostics"
                )));
            }
            let actionable_version = match (opened_version, published_version) {
                | (Some(opened), Some(published)) if opened == published => Some(opened),
                | (Some(0), None) => Some(0),
                | _ => None,
            };
            let residual_bytes = diagnostic_entry_retained_bytes(uri, diagnostics)?;
            let previous_residual_bytes = self
                .diagnostics
                .get(uri)
                .map_or(Ok(0), |values| diagnostic_entry_retained_bytes(uri, values))?;
            let actionable_bytes = actionable_version
                .map_or(Ok(0), |_| actionable_entry_retained_bytes(uri, diagnostics))?;
            let previous_actionable_bytes = self
                .actionable_diagnostics
                .get(uri)
                .map_or(Ok(0), |values| actionable_entry_retained_bytes(uri, values))?;
            let removed_bytes = previous_residual_bytes
                .checked_add(previous_actionable_bytes)
                .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
            let added_bytes = residual_bytes
                .checked_add(actionable_bytes)
                .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
            let published_version_bytes =
                if published_version.is_some() && !self.published_versions.contains_key(uri) {
                    uri_version_entry_retained_bytes(uri)?
                }
                else {
                    0
                };
            let added_bytes = added_bytes
                .checked_add(published_version_bytes)
                .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
            let retained_bytes = replace_retained_bytes(
                self.diagnostic_bytes,
                removed_bytes,
                added_bytes,
                "LSP diagnostic",
            )?;
            if retained_bytes > MAX_RETAINED_DIAGNOSTIC_BYTES {
                return Err(AifixError::process(format!(
                    "LSP session exceeded {MAX_RETAINED_DIAGNOSTIC_BYTES} retained diagnostic bytes"
                )));
            }
            self.diagnostics.insert(uri.to_owned(), diagnostics.clone());
            if let Some(actionable_version) = actionable_version {
                self.actionable_diagnostics
                    .insert(uri.to_owned(), diagnostics.clone());
                self.diagnostic_versions
                    .insert(uri.to_owned(), actionable_version);
            }
            else {
                self.actionable_diagnostics.remove(uri);
                self.diagnostic_versions.remove(uri);
            }
            if let Some(published_version) = published_version {
                self.published_versions
                    .insert(uri.to_owned(), published_version);
            }
            self.diagnostic_bytes = retained_bytes;
            self.bump_diagnostic_generation()?;
            if opened_version
                .is_some_and(|opened| published_version.is_none_or(|published| published == opened))
            {
                self.pending_diagnostics.remove(uri);
            }
        }
        Ok(())
    }

    /// Advance the diagnostic-state generation without saturation.
    fn bump_diagnostic_generation(&mut self) -> Result<(), AifixError>
    {
        self.diagnostic_generation = self
            .diagnostic_generation
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP diagnostic generation overflowed"))?;
        Ok(())
    }

    /// Send one JSON-RPC notification.
    fn notify(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<(), AifixError>
    {
        let deadline = self.operation_deadline()?;
        self.send(
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            deadline,
        )
    }

    /// Send one successful JSON-RPC response.
    fn respond(
        &mut self,
        id: &Value,
        result: &Value,
    ) -> Result<(), AifixError>
    {
        let deadline = self.operation_deadline()?;
        self.send(
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            deadline,
        )
    }

    /// Send the final `exit` notification without treating the expected EOF as
    /// a protocol failure while the writer acknowledges the frame.
    fn notify_exit(&self) -> Result<(), AifixError>
    {
        let deadline = self.operation_deadline()?;
        let payload = serde_json::to_vec(
            &json!({ "jsonrpc": "2.0", "method": "exit", "params": Value::Null }),
        )?;
        let (result, acknowledgement) = mpsc::channel();
        let mut request = WriteRequest { payload, result };
        loop {
            match self.writer.try_send(request) {
                | Ok(()) => break,
                | Err(TrySendError::Full(returned)) => {
                    request = returned;
                    if Instant::now() >= deadline {
                        return Err(AifixError::process(
                            "LSP writer queue remained full while sending exit",
                        ));
                    }
                    thread::sleep(Duration::from_millis(1));
                },
                | Err(TrySendError::Disconnected(_)) => {
                    return Err(AifixError::process(
                        "LSP writer disconnected before accepting exit",
                    ));
                },
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match acknowledgement.recv_timeout(remaining) {
            | Ok(Ok(())) => Ok(()),
            | Ok(Err(error)) => Err(AifixError::process(format!(
                "failed to write LSP exit notification: {error}"
            ))),
            | Err(RecvTimeoutError::Timeout) => {
                Err(AifixError::process("LSP exit notification write timed out"))
            },
            | Err(RecvTimeoutError::Disconnected) => Err(AifixError::process(
                "LSP writer disconnected before acknowledging exit",
            )),
        }
    }

    /// Return a deadline bounded by the complete LSP session.
    fn operation_deadline(&self) -> Result<Instant, AifixError>
    {
        let now = Instant::now();
        if now >= self.session_deadline {
            return Err(AifixError::process(format!(
                "LSP code-action session timed out after {} ms{}",
                self.config.timeout.as_millis(),
                self.stderr_suffix()
            )));
        }
        Ok(self.session_deadline.min(now + self.config.timeout))
    }

    /// Serialize and frame one JSON-RPC message while servicing server traffic
    /// that could otherwise block the server's stdout and stdin.
    fn send(
        &mut self,
        message: &Value,
        deadline: Instant,
    ) -> Result<(), AifixError>
    {
        let payload = serde_json::to_vec(message)?;
        if payload.len() > MAX_LSP_MESSAGE_BYTES {
            return Err(AifixError::process(format!(
                "outgoing LSP message exceeded {MAX_LSP_MESSAGE_BYTES} bytes"
            )));
        }
        let (result, acknowledgement) = mpsc::channel();
        let mut request = WriteRequest { payload, result };
        loop {
            match self.writer.try_send(request) {
                | Ok(()) => break,
                | Err(TrySendError::Full(returned)) => {
                    request = returned;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        self.terminate_child();
                        return Err(AifixError::process(format!(
                            "LSP writer queue remained full until the session deadline{}",
                            self.stderr_suffix()
                        )));
                    }
                    self.pump_during_write(remaining.min(Duration::from_millis(1)))?;
                },
                | Err(TrySendError::Disconnected(_returned)) => {
                    return Err(AifixError::process(format!(
                        "LSP writer disconnected before accepting a message{}",
                        self.stderr_suffix()
                    )));
                },
            }
        }
        loop {
            match acknowledgement.try_recv() {
                | Ok(Ok(())) => return Ok(()),
                | Ok(Err(error)) => {
                    return Err(AifixError::process(format!(
                        "failed to write LSP request: {error}{}",
                        self.stderr_suffix()
                    )));
                },
                | Err(TryRecvError::Disconnected) => {
                    return Err(AifixError::process(format!(
                        "LSP writer disconnected before acknowledging a message{}",
                        self.stderr_suffix()
                    )));
                },
                | Err(TryRecvError::Empty) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        self.terminate_child();
                        return Err(AifixError::process(format!(
                            "LSP write timed out after {} ms{}",
                            self.config.timeout.as_millis(),
                            self.stderr_suffix()
                        )));
                    }
                    self.pump_during_write(remaining.min(Duration::from_millis(1)))?;
                },
            }
        }
    }

    /// Service one server event while a child-stdin write is pending, deferring
    /// the active client request's response for the request loop.
    fn pump_during_write(
        &mut self,
        wait: Duration,
    ) -> Result<(), AifixError>
    {
        match self.messages.recv_timeout(wait) {
            | Ok(event) => {
                let defer_until_state_transition =
                    event.as_ref().is_ok_and(|message| {
                        match message.get("method").and_then(Value::as_str) {
                            | None => self.pending_request_id.is_some(),
                            | Some("textDocument/publishDiagnostics" | "workspace/applyEdit") => {
                                true
                            },
                            | Some(_) => false,
                        }
                    });
                if defer_until_state_transition {
                    self.defer_event(event)
                }
                else {
                    self.handle_event(&event, None).map(drop)
                }
            },
            | Err(RecvTimeoutError::Timeout) => Ok(()),
            | Err(RecvTimeoutError::Disconnected) => Err(self.disconnected_error()),
        }
    }
    /// Retain one state-sensitive event under a decoded-byte budget.
    fn defer_event(
        &mut self,
        event: ReaderEvent,
    ) -> Result<(), AifixError>
    {
        if self.deferred_events.len() >= MAX_DEFERRED_LSP_EVENTS {
            return Err(AifixError::process(format!(
                "LSP session exceeded {MAX_DEFERRED_LSP_EVENTS} deferred events"
            )));
        }
        let bytes = reader_event_retained_bytes(&event)?;
        let retained = self
            .deferred_event_bytes
            .checked_add(bytes)
            .ok_or_else(|| AifixError::process("LSP deferred-event byte accounting overflowed"))?;
        if retained > MAX_DEFERRED_LSP_BYTES {
            return Err(AifixError::process(format!(
                "LSP session exceeded {MAX_DEFERRED_LSP_BYTES} deferred decoded bytes"
            )));
        }
        self.deferred_events.push_back(event);
        self.deferred_event_bytes = retained;
        Ok(())
    }

    /// Remove one deferred event and release its decoded-byte accounting.
    fn pop_deferred_event(&mut self) -> Result<Option<ReaderEvent>, AifixError>
    {
        let Some(event) = self.deferred_events.pop_front()
        else {
            return Ok(None);
        };
        self.deferred_event_bytes = self
            .deferred_event_bytes
            .checked_sub(reader_event_retained_bytes(&event)?)
            .ok_or_else(|| AifixError::process("LSP deferred-event byte accounting underflowed"))?;
        Ok(Some(event))
    }

    /// Restore every source changed by this session after a terminal failure.
    fn rollback_committed_edits(
        &mut self,
        error: AifixError,
    ) -> AifixError
    {
        self.committed_edit_bytes = 0;
        let committed = core::mem::take(&mut self.committed_edits);
        if committed.is_empty() {
            error
        }
        else {
            with_workspace_rollback(error, &committed)
        }
    }

    /// Stop the server through the LSP lifecycle, then reap it.
    fn shutdown(&mut self) -> Result<(), AifixError>
    {
        if self.stopped {
            return Ok(());
        }
        self.request("shutdown", &Value::Null)?;
        self.notify_exit()?;
        let deadline = self.session_deadline;
        loop {
            let status = self
                .child
                .as_mut()
                .ok_or_else(|| AifixError::process("LSP server process was not available"))?
                .try_wait();
            match status {
                | Ok(Some(status)) => {
                    self.child.take();
                    self.stopped = true;
                    if status.success() {
                        return Ok(());
                    }
                    return Err(AifixError::process(format!(
                        "LSP server exited with status {status}{}",
                        self.stderr_suffix()
                    )));
                },
                | Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                },
                | Ok(None) => {
                    self.terminate_child();
                    return Err(AifixError::process(
                        "LSP server did not exit before the session deadline",
                    ));
                },
                | Err(error) => {
                    self.terminate_child();
                    return Err(AifixError::process(format!(
                        "failed to wait for LSP server: {error}"
                    )));
                },
            }
        }
    }

    /// Kill the child promptly and hand potentially blocking reaping to a
    /// detached helper thread.
    fn terminate_child(&mut self)
    {
        let Some(mut child) = self.child.take()
        else {
            self.stopped = true;
            return;
        };
        kill_server_process_tree(&mut child);
        drop(
            thread::Builder::new()
                .name("aifix-lsp-reaper".to_owned())
                .spawn(move || drop(child.wait())),
        );
        self.stopped = true;
    }

    /// Render bounded server stderr as optional process context.
    fn stderr_suffix(&self) -> String
    {
        let Ok(stderr) = self.stderr.lock()
        else {
            return String::new();
        };
        if stderr.is_empty() {
            String::new()
        }
        else {
            format!("; stderr: {}", String::from_utf8_lossy(&stderr))
        }
    }

    /// Build an EOF/disconnection error with the child status when available.
    fn disconnected_error(&mut self) -> AifixError
    {
        let status = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
            .map_or_else(|| "before exiting".to_owned(), |status| status.to_string());
        AifixError::process(format!(
            "LSP server disconnected {status}{}",
            self.stderr_suffix()
        ))
    }
}

impl Drop for LspSession
{
    fn drop(&mut self)
    {
        if !self.stopped {
            self.terminate_child();
        }
    }
}

/// Return a structurally valid LSP `Command` from a direct command or
/// `CodeAction`.
fn command_payload(action: &Value) -> Option<&Value>
{
    let command = action.get("command")?;
    let payload = if command.is_string() {
        action
    }
    else if command.is_object() {
        command
    }
    else {
        return None;
    };
    payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|identifier| !identifier.is_empty())?;
    if payload
        .get("arguments")
        .is_some_and(|value| !value.is_array())
    {
        return None;
    }
    Some(payload)
}

/// Return the exact identifier from a structurally valid LSP Command.
fn command_identifier(action: &Value) -> Option<&str>
{
    command_payload(action)?.get("command")?.as_str()
}

/// Stable action identity without serializing raw payloads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ActionKey
{
    /// URI of the correlated diagnostic document.
    uri: String,
    /// Human-readable action title.
    title: String,
    /// Hierarchical LSP action kind.
    kind: String,
    /// Optional exact server command identifier.
    command: Option<String>,
    /// Full normalized identity of the correlated diagnostic.
    diagnostic: Arc<DiagnosticKey>,
}

impl ActionKey
{
    /// Build an action key from semantic fields and diagnostic location.
    fn from_action(
        uri: &str,
        diagnostic: &Arc<DiagnosticKey>,
        action: &Value,
    ) -> Self
    {
        let command = command_identifier(action).map(ToOwned::to_owned);
        Self {
            uri: uri.to_owned(),
            title: action
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("untitled code action")
                .to_owned(),
            kind: action
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            command,
            diagnostic: Arc::clone(diagnostic),
        }
    }

    /// Estimate heap bytes retained while this key remains in loop history.
    fn retained_bytes(&self) -> Result<usize, AifixError>
    {
        self.retained_bytes_without_diagnostic()?
            .checked_add(self.diagnostic.retained_bytes()?)
            .ok_or_else(|| AifixError::process("LSP action-key byte accounting overflowed"))
    }

    /// Estimate key bytes excluding the shared correlated diagnostic.
    fn retained_bytes_without_diagnostic(&self) -> Result<usize, AifixError>
    {
        [self.uri.len(), self.title.len(), self.kind.len()]
            .into_iter()
            .try_fold(256_usize, usize::checked_add)
            .and_then(|bytes| {
                self.command
                    .as_ref()
                    .map_or(Some(bytes), |command| bytes.checked_add(command.len()))
            })
            .ok_or_else(|| AifixError::process("LSP action-key byte accounting overflowed"))
    }
}

/// Stable structural identity for one published diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DiagnosticKey(Arc<CanonicalJson>);

impl DiagnosticKey
{
    /// Build a key from every published diagnostic field, including opaque
    /// `data` that servers use to correlate later code actions.
    fn from_diagnostic(diagnostic: &Value) -> Self
    {
        Self(Arc::new(CanonicalJson::from_value(diagnostic)))
    }

    /// Estimate the shared diagnostic identity's retained heap bytes.
    fn retained_bytes(&self) -> Result<usize, AifixError>
    {
        self.0
            .retained_bytes()?
            .checked_add(32)
            .ok_or_else(|| AifixError::process("LSP action-key byte accounting overflowed"))
    }
}

/// Recursively comparable JSON identity without serializing raw payloads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalJson
{
    /// JSON null.
    Null,
    /// JSON Boolean.
    Bool(bool),
    /// JSON number in `serde_json`'s normalized lexical form.
    Number(String),
    /// JSON string.
    String(String),
    /// Ordered JSON array.
    Array(Vec<Self>),
    /// Key-sorted JSON object.
    Object(Vec<(String, Self)>),
}

impl CanonicalJson
{
    /// Convert arbitrary JSON into a deterministic structural identity.
    fn from_value(value: &Value) -> Self
    {
        match *value {
            | Value::Null => Self::Null,
            | Value::Bool(value) => Self::Bool(value),
            | Value::Number(ref number) => Self::Number(number.to_string()),
            | Value::String(ref text) => Self::String(text.clone()),
            | Value::Array(ref values) => {
                Self::Array(values.iter().map(Self::from_value).collect())
            },
            | Value::Object(ref fields) => {
                let mut fields = fields
                    .iter()
                    .map(|field| (field.0.clone(), Self::from_value(field.1)))
                    .collect::<Vec<_>>();
                fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                Self::Object(fields)
            },
        }
    }

    /// Estimate heap bytes retained by this canonical JSON identity.
    fn retained_bytes(&self) -> Result<usize, AifixError>
    {
        match *self {
            | Self::Null | Self::Bool(_) => Ok(JSON_VALUE_OVERHEAD_BYTES),
            | Self::Number(ref value) | Self::String(ref value) => JSON_VALUE_OVERHEAD_BYTES
                .checked_add(value.len())
                .ok_or_else(|| AifixError::process("LSP action-key byte accounting overflowed")),
            | Self::Array(ref values) => {
                values
                    .iter()
                    .try_fold(JSON_VALUE_OVERHEAD_BYTES, |total, value| {
                        total.checked_add(value.retained_bytes()?).ok_or_else(|| {
                            AifixError::process("LSP action-key byte accounting overflowed")
                        })
                    })
            },
            | Self::Object(ref fields) => {
                fields
                    .iter()
                    .try_fold(JSON_VALUE_OVERHEAD_BYTES, |total, field| {
                        let field_bytes = MAP_ENTRY_OVERHEAD_BYTES
                            .checked_add(field.0.len())
                            .and_then(|bytes| bytes.checked_add(field.1.retained_bytes().ok()?))
                            .ok_or_else(|| {
                                AifixError::process("LSP action-key byte accounting overflowed")
                            })?;
                        total.checked_add(field_bytes).ok_or_else(|| {
                            AifixError::process("LSP action-key byte accounting overflowed")
                        })
                    })
            },
        }
    }
}

/// One filtered action plus its deterministic selection metadata.
#[derive(Debug, Clone)]
struct SelectedAction
{
    /// Stable identity used for ordering and loop detection.
    key: ActionKey,
    /// Whether the server marked this action preferred.
    preferred: bool,
    /// Server-owned action payload applied within this session only.
    value: Value,
}

/// Parse an optional preferred marker without accepting malformed values.
fn action_preferred(action: &Value) -> Result<bool, AifixError>
{
    let Some(preferred) = action.get("isPreferred")
    else {
        return Ok(false);
    };
    preferred
        .as_bool()
        .ok_or_else(|| AifixError::process("LSP code action isPreferred field was not a boolean"))
}

/// Return whether an action's optional diagnostic list contains the request
/// diagnostic; omitted lists inherit the request correlation.
fn action_matches_diagnostic(
    action: &Value,
    expected: &DiagnosticKey,
) -> Result<bool, AifixError>
{
    let Some(value) = action.get("diagnostics")
    else {
        return Ok(true);
    };
    let diagnostics = value
        .as_array()
        .ok_or_else(|| AifixError::process("LSP code action diagnostics field was not an array"))?;
    for candidate in diagnostics {
        if !candidate.is_object() {
            return Err(AifixError::process(
                "LSP code action diagnostics array contained a non-object value",
            ));
        }
        if &DiagnosticKey::from_diagnostic(candidate) == expected {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Return whether resolution preserved immutable action-selection metadata.
fn resolved_metadata_preserved(
    unresolved: &Value,
    resolved: &Value,
) -> bool
{
    ["title", "kind", "isPreferred", "diagnostics"]
        .into_iter()
        .all(|field| unresolved.get(field) == resolved.get(field))
        && unresolved
            .get("command")
            .is_none_or(|command| resolved.get("command") == Some(command))
}

/// Collect text edits from both `WorkspaceEdit` representations.
fn collect_workspace_edits(edit: &Value) -> Result<BTreeMap<String, Vec<Value>>, AifixError>
{
    let mut edits = BTreeMap::<String, Vec<Value>>::new();
    if edit.get("changes").is_some() && edit.get("documentChanges").is_some() {
        return Err(AifixError::invalid_argument(
            "automatic LSP workspace edits cannot combine changes and documentChanges",
        ));
    }
    if let Some(changes) = edit.get("changes") {
        let changes = changes
            .as_object()
            .ok_or_else(|| AifixError::process("LSP WorkspaceEdit.changes was not an object"))?;
        for (uri, values) in changes {
            let values = values.as_array().ok_or_else(|| {
                AifixError::process("LSP WorkspaceEdit.changes entry was not an array")
            })?;
            edits.entry(uri.clone()).or_default().extend(values.clone());
            validate_edit_annotations(edit, values)?;
        }
    }
    if let Some(document_changes) = edit.get("documentChanges") {
        let document_changes = document_changes.as_array().ok_or_else(|| {
            AifixError::process("LSP WorkspaceEdit.documentChanges was not an array")
        })?;
        for change in document_changes {
            if change.get("kind").is_some() {
                return Err(AifixError::invalid_argument(
                    "LSP resource create, rename, and delete operations are not automatic-safe",
                ));
            }
            let uri = change
                .get("textDocument")
                .and_then(|document| document.get("uri"))
                .and_then(Value::as_str)
                .ok_or_else(|| AifixError::process("LSP TextDocumentEdit had no document URI"))?;
            let values = change
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| AifixError::process("LSP TextDocumentEdit had no edit array"))?;
            if edits.contains_key(uri) {
                return Err(AifixError::invalid_argument(format!(
                    "automatic LSP workspace edit repeated document `{uri}`"
                )));
            }
            validate_edit_annotations(edit, values)?;
            edits
                .entry(uri.to_owned())
                .or_default()
                .extend(values.clone());
        }
    }
    if edits.is_empty() {
        return Err(AifixError::process(
            "LSP code action WorkspaceEdit contained no text edits",
        ));
    }
    let edit_count = edits.values().try_fold(0_usize, |total, values| {
        total
            .checked_add(values.len())
            .ok_or_else(|| AifixError::process("LSP workspace edit count overflowed"))
    })?;
    if edit_count == 0 {
        return Err(AifixError::invalid_argument(
            "automatic LSP workspace edit contained no text edits",
        ));
    }
    if edit_count > MAX_WORKSPACE_TEXT_EDITS {
        return Err(AifixError::process(format!(
            "LSP workspace edit exceeded {MAX_WORKSPACE_TEXT_EDITS} text edits"
        )));
    }
    Ok(edits)
}

/// Validate annotated text edits and reject any requiring user confirmation.
fn validate_edit_annotations(
    workspace_edit: &Value,
    edits: &[Value],
) -> Result<(), AifixError>
{
    for edit in edits {
        let annotation_id = match edit.get("annotationId") {
            | None => continue,
            | Some(value) => value
                .as_str()
                .filter(|annotation_id| !annotation_id.is_empty())
                .ok_or_else(|| {
                    AifixError::process("LSP TextEdit.annotationId was not a nonempty string")
                })?,
        };
        let annotations = workspace_edit
            .get("changeAnnotations")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AifixError::process(
                    "LSP annotated text edit requires an object changeAnnotations map",
                )
            })?;
        let annotation = annotations
            .get(annotation_id)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AifixError::process(format!(
                    "LSP annotated text edit referenced unknown or malformed annotation \
                     `{annotation_id}`"
                ))
            })?;
        if annotation
            .get("label")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(AifixError::process(format!(
                "LSP text edit annotation `{annotation_id}` requires a nonempty label"
            )));
        }
        let needs_confirmation = match annotation.get("needsConfirmation") {
            | None => false,
            | Some(&Value::Bool(value)) => value,
            | Some(_) => {
                return Err(AifixError::process(format!(
                    "LSP text edit annotation `{annotation_id}` needsConfirmation was not a boolean"
                )));
            },
        };
        if needs_confirmation {
            return Err(AifixError::invalid_argument(format!(
                "LSP text edit annotation `{annotation_id}` requires confirmation"
            )));
        }
    }
    Ok(())
}

/// Apply validated non-overlapping LSP text edits in one forward pass.
fn apply_text_edits(
    text: &str,
    edits: &[Value],
    deadline: Instant,
) -> Result<String, AifixError>
{
    let mut parsed = Vec::with_capacity(edits.len());
    let mut positions = Vec::with_capacity(edits.len().saturating_mul(2));
    for edit in edits {
        ensure_deadline(deadline, "LSP text-edit validation")?;
        let range = edit
            .get("range")
            .ok_or_else(|| AifixError::process("LSP TextEdit had no range"))?;
        let start = parse_position(
            range
                .get("start")
                .ok_or_else(|| AifixError::process("LSP TextEdit range had no start position"))?,
        )?;
        let end = parse_position(
            range
                .get("end")
                .ok_or_else(|| AifixError::process("LSP TextEdit range had no end position"))?,
        )?;
        let new_text = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| AifixError::process("LSP TextEdit had no string newText"))?;
        positions.push(start);
        positions.push(end);
        parsed.push((start, end, new_text));
    }
    positions.sort_unstable();
    positions.dedup();
    let offsets = resolve_position_offsets(text, &positions, deadline)?;
    let mut resolved = parsed
        .into_iter()
        .map(|(start, end, new_text)| {
            let start = *offsets
                .get(&start)
                .ok_or_else(|| AifixError::process("LSP start position was not resolved"))?;
            let end = *offsets
                .get(&end)
                .ok_or_else(|| AifixError::process("LSP end position was not resolved"))?;
            if start > end {
                return Err(AifixError::invalid_argument(
                    "LSP TextEdit range ended before it started",
                ));
            }
            Ok((start, end, new_text))
        })
        .collect::<Result<Vec<_>, AifixError>>()?;
    resolved.sort_by_key(|edit| edit.0);
    let added_bytes = resolved.iter().try_fold(0_usize, |total, edit| {
        total
            .checked_add(edit.2.len())
            .ok_or_else(|| AifixError::process("LSP edited document length overflowed"))
    })?;
    let mut updated = String::with_capacity(
        text.len()
            .checked_add(added_bytes)
            .ok_or_else(|| AifixError::process("LSP edited document length overflowed"))?,
    );
    let mut cursor = 0_usize;
    for (start, end, replacement) in resolved {
        ensure_deadline(deadline, "LSP text-edit application")?;
        if start < cursor {
            return Err(AifixError::invalid_argument(
                "LSP workspace edit contained overlapping text edits",
            ));
        }
        updated.push_str(text.get(cursor .. start).ok_or_else(|| {
            AifixError::invalid_argument("LSP text edit did not resolve to UTF-8 boundaries")
        })?);
        updated.push_str(replacement);
        cursor = end;
    }
    updated.push_str(text.get(cursor ..).ok_or_else(|| {
        AifixError::invalid_argument("LSP text edit tail did not start on a UTF-8 boundary")
    })?);
    Ok(updated)
}

/// Parse one nonnegative LSP line/UTF-16 character position.
fn parse_position(position: &Value) -> Result<(usize, usize), AifixError>
{
    Ok((
        json_usize(position.get("line"), "line")?,
        json_usize(position.get("character"), "character")?,
    ))
}

/// Resolve sorted unique LSP positions with one source scan and one scan per
/// referenced line.
fn resolve_position_offsets(
    text: &str,
    positions: &[(usize, usize)],
    deadline: Instant,
) -> Result<BTreeMap<(usize, usize), usize>, AifixError>
{
    let lines = requested_line_ranges(text, positions, deadline)?;
    let mut offsets = BTreeMap::new();
    let mut position_index = 0_usize;
    while let Some(&(line, _)) = positions.get(position_index) {
        ensure_deadline(deadline, "LSP position resolution")?;
        let (line_start, line_end) = *lines.get(&line).ok_or_else(|| {
            AifixError::invalid_argument("LSP position line exceeded document line count")
        })?;
        let mut line_positions_end = position_index + 1;
        while positions
            .get(line_positions_end)
            .is_some_and(|position| position.0 == line)
        {
            line_positions_end += 1;
        }
        let mut target_index = position_index;
        let mut utf16 = 0_usize;
        let mut byte_offset = line_start;
        while let Some(position) = positions
            .get(target_index)
            .copied()
            .filter(|position| position.1 == 0)
        {
            offsets.insert(position, byte_offset);
            target_index += 1;
        }
        let line_text = text.get(line_start .. line_end).ok_or_else(|| {
            AifixError::process("LSP line range did not align to UTF-8 boundaries")
        })?;
        for (scalar_index, scalar) in line_text.chars().enumerate() {
            if scalar_index % 4096 == 0 {
                ensure_deadline(deadline, "LSP position resolution")?;
            }
            utf16 = utf16
                .checked_add(scalar.len_utf16())
                .ok_or_else(|| AifixError::process("LSP UTF-16 position overflowed"))?;
            byte_offset = byte_offset
                .checked_add(scalar.len_utf8())
                .ok_or_else(|| AifixError::process("LSP byte position overflowed"))?;
            while let Some(position) = positions
                .get(target_index)
                .copied()
                .filter(|position| position.1 == utf16)
            {
                offsets.insert(position, byte_offset);
                target_index += 1;
            }
            if positions
                .get(target_index)
                .is_some_and(|position| position.1 < utf16)
            {
                return Err(AifixError::invalid_argument(
                    "LSP position split a UTF-16 surrogate pair",
                ));
            }
        }
        if target_index < line_positions_end {
            return Err(AifixError::invalid_argument(
                "LSP position character exceeded line length",
            ));
        }
        position_index = line_positions_end;
    }
    Ok(offsets)
}

/// Return byte ranges only for referenced logical lines.
fn requested_line_ranges(
    text: &str,
    positions: &[(usize, usize)],
    deadline: Instant,
) -> Result<BTreeMap<usize, (usize, usize)>, AifixError>
{
    let mut requested = positions
        .iter()
        .map(|position| position.0)
        .collect::<Vec<_>>();
    requested.sort_unstable();
    requested.dedup();
    let bytes = text.as_bytes();
    let mut ranges = BTreeMap::new();
    let mut target = 0_usize;
    let mut line = 0_usize;
    let mut start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() && target < requested.len() {
        if index.is_multiple_of(64 * 1024) {
            ensure_deadline(deadline, "LSP line indexing")?;
        }
        let Some(byte) = bytes.get(index)
        else {
            break;
        };
        let line_end = match *byte {
            | b'\r' | b'\n' => Some(index),
            | _ => None,
        };
        let Some(end) = line_end
        else {
            index += 1;
            continue;
        };
        if requested.get(target).copied() == Some(line) {
            ranges.insert(line, (start, end));
            target += 1;
        }
        index += 1;
        if bytes.get(end) == Some(&b'\r') && bytes.get(index) == Some(&b'\n') {
            index += 1;
        }
        start = index;
        line = line
            .checked_add(1)
            .ok_or_else(|| AifixError::process("LSP document line count overflowed"))?;
    }
    if requested.get(target).copied() == Some(line) {
        ranges.insert(line, (start, bytes.len()));
        target += 1;
    }
    if target != requested.len() {
        return Err(AifixError::invalid_argument(
            "LSP position line exceeded document line count",
        ));
    }
    Ok(ranges)
}

/// Reject local computation that outlives the complete session.
fn ensure_deadline(
    deadline: Instant,
    operation: &str,
) -> Result<(), AifixError>
{
    if Instant::now() >= deadline {
        return Err(AifixError::process(format!(
            "{operation} exceeded the LSP session deadline"
        )));
    }
    Ok(())
}

/// Read one nonnegative JSON integer as usize.
fn json_usize(
    value: Option<&Value>,
    field: &str,
) -> Result<usize, AifixError>
{
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| AifixError::process(format!("LSP position {field} was not a usize")))
}

/// Validate the server's code-action provider union and return resolve support.
fn resolve_code_action_provider(result: &Value) -> Result<bool, AifixError>
{
    let provider = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("codeActionProvider"))
        .ok_or_else(|| {
            AifixError::invalid_argument("LSP server did not advertise code-action support")
        })?;
    if let Some(supported) = provider.as_bool() {
        return supported.then_some(false).ok_or_else(|| {
            AifixError::invalid_argument("LSP server does not support code actions")
        });
    }
    let options = provider.as_object().ok_or_else(|| {
        AifixError::process("LSP codeActionProvider was not a boolean or options object")
    })?;
    let Some(resolve_provider) = options.get("resolveProvider")
    else {
        return Ok(false);
    };
    resolve_provider.as_bool().ok_or_else(|| {
        AifixError::process("LSP codeActionProvider.resolveProvider was not a boolean")
    })
}

/// Require every configured automatic command to be advertised by the server.
fn validate_execute_command_provider(
    result: &Value,
    allowed_commands: &[String],
) -> Result<(), AifixError>
{
    if allowed_commands.is_empty() {
        return Ok(());
    }
    let commands = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("executeCommandProvider"))
        .and_then(|provider| provider.get("commands"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AifixError::invalid_argument(
                "LSP server did not advertise commands required by code_actions.allowed_commands",
            )
        })?;
    let advertised = commands
        .iter()
        .map(|command| {
            command.as_str().ok_or_else(|| {
                AifixError::process(
                    "LSP executeCommandProvider.commands contained a non-string value",
                )
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if let Some(missing) = allowed_commands
        .iter()
        .find(|command| !advertised.contains(command.as_str()))
    {
        return Err(AifixError::invalid_argument(format!(
            "LSP server did not advertise allowlisted command `{missing}`"
        )));
    }
    Ok(())
}

/// Resolve the synchronization kind selected by the initialized server.
fn resolve_text_document_sync(result: &Value) -> Result<TextDocumentSync, AifixError>
{
    let capability = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("textDocumentSync"))
        .ok_or_else(|| {
            AifixError::invalid_argument(
                "LSP server did not advertise text document synchronization",
            )
        })?;
    if let Some(options) = capability.as_object() {
        match options.get("openClose") {
            | Some(&Value::Bool(true)) => {},
            | Some(&Value::Bool(false)) | None => {
                return Err(AifixError::invalid_argument(
                    "LSP server does not advertise document open/close synchronization",
                ));
            },
            | Some(_) => {
                return Err(AifixError::process(
                    "LSP textDocumentSync.openClose was not a boolean",
                ));
            },
        }
    }
    let kind = capability
        .as_u64()
        .or_else(|| capability.get("change").and_then(Value::as_u64));
    match kind {
        | Some(1) => Ok(TextDocumentSync::Full),
        | Some(2) => Ok(TextDocumentSync::Incremental),
        | Some(0) => Err(AifixError::invalid_argument(
            "LSP server does not support document change synchronization",
        )),
        | Some(other) => Err(AifixError::invalid_argument(format!(
            "LSP server advertised unsupported text synchronization kind {other}"
        ))),
        | None => Err(AifixError::invalid_argument(
            "LSP server text synchronization capability had no supported change kind",
        )),
    }
}

/// Return the UTF-16 LSP position immediately after a document's final scalar.
fn document_end_position(
    text: &str,
    deadline: Instant,
) -> Result<(u64, u64), AifixError>
{
    let mut line = 0_u64;
    let mut character = 0_u64;
    let mut scalars = text.chars().peekable();
    let mut scalar_index = 0_usize;
    while let Some(scalar) = scalars.next() {
        if scalar_index.is_multiple_of(4096) {
            ensure_deadline(deadline, "LSP document end-position calculation")?;
        }
        scalar_index = scalar_index.saturating_add(1);
        match scalar {
            | '\r' => {
                if scalars.peek() == Some(&'\n') {
                    scalars.next();
                }
                line = line
                    .checked_add(1)
                    .ok_or_else(|| AifixError::process("LSP document line count overflowed"))?;
                character = 0;
            },
            | '\n' => {
                line = line
                    .checked_add(1)
                    .ok_or_else(|| AifixError::process("LSP document line count overflowed"))?;
                character = 0;
            },
            | _ => {
                character = character
                    .checked_add(u64::try_from(scalar.len_utf16()).map_err(|error| {
                        AifixError::process(format!(
                            "LSP document UTF-16 scalar length overflowed: {error}"
                        ))
                    })?)
                    .ok_or_else(|| {
                        AifixError::process("LSP document UTF-16 position overflowed")
                    })?;
            },
        }
    }
    Ok((line, character))
}

/// Atomically replace one source file after two synchronized-content checks.
fn atomic_replace_if_unchanged(
    path: &Utf8Path,
    expected: &str,
    updated: &str,
) -> Result<(), AifixError>
{
    let temporary = stage_atomic_replacement(path, expected, updated)?;
    replace_staged_if_unchanged(path, expected, &temporary)
}

/// Stage a complete same-directory replacement and preserve target permissions.
fn stage_atomic_replacement(
    path: &Utf8Path,
    expected: &str,
    updated: &str,
) -> Result<Utf8PathBuf, AifixError>
{
    stage_atomic_replacement_with(path, expected, updated, |file, replacement| {
        file.write_all(replacement.as_bytes())?;
        file.flush()?;
        file.sync_all()
    })
}

/// Stage and atomically replace a source file through an injected writer.
#[cfg(test)]
fn atomic_replace_with(
    path: &Utf8Path,
    expected: &str,
    updated: &str,
    stage: impl FnOnce(&mut fs::File, &str) -> io::Result<()>,
) -> Result<(), AifixError>
{
    let temporary = stage_atomic_replacement_with(path, expected, updated, stage)?;
    replace_staged_if_unchanged(path, expected, &temporary)
}

/// Write one complete replacement beside its target without mutating the
/// target.
fn stage_atomic_replacement_with(
    path: &Utf8Path,
    expected: &str,
    updated: &str,
    stage: impl FnOnce(&mut fs::File, &str) -> io::Result<()>,
) -> Result<Utf8PathBuf, AifixError>
{
    #[cfg(windows)]
    {
        drop((path, expected, updated, stage));
        Err(AifixError::invalid_argument(
            "automatic LSP workspace edits are disabled on Windows because atomic replacement \
             cannot preserve the target security descriptor",
        ))
    }
    #[cfg(not(windows))]
    {
        stage_atomic_replacement_with_metadata(path, expected, updated, stage)
    }
}

/// Stage a same-directory replacement with verified security metadata.
#[cfg(not(windows))]
fn stage_atomic_replacement_with_metadata(
    path: &Utf8Path,
    expected: &str,
    updated: &str,
    stage: impl FnOnce(&mut fs::File, &str) -> io::Result<()>,
) -> Result<Utf8PathBuf, AifixError>
{
    let metadata =
        fs::metadata(path).map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    validate_replacement_metadata(path, &metadata)?;
    if !file_matches_expected(path, expected.as_bytes())? {
        return Err(AifixError::invalid_argument(format!(
            "LSP workspace edit target changed before replacement: `{path}`"
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        AifixError::invalid_argument(format!("LSP workspace edit target had no parent: `{path}`"))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        AifixError::invalid_argument(format!(
            "LSP workspace edit target had no file name: `{path}`"
        ))
    })?;
    let mut staged = None;
    for _ in 0 .. MAX_TEMP_FILE_ATTEMPTS {
        let suffix = secure_temporary_suffix()?;
        let candidate = parent.join(format!(".{file_name}.aifix-{suffix}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&candidate) {
            | Ok(file) => {
                staged = Some((candidate, file));
                break;
            },
            | Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
            | Err(error) => return Err(AifixError::io_path(candidate, error)),
        }
    }
    let (temporary, mut file) = staged.ok_or_else(|| {
        AifixError::process(format!(
            "could not stage atomic LSP edit after {MAX_TEMP_FILE_ATTEMPTS} attempts"
        ))
    })?;
    let result = (|| {
        stage(&mut file, updated).map_err(|error| AifixError::io_path(temporary.clone(), error))?;
        file.sync_all()
            .map_err(|error| AifixError::io_path(temporary.clone(), error))?;
        file.set_permissions(metadata.permissions())
            .map_err(|error| AifixError::io_path(temporary.clone(), error))?;
        copy_security_metadata(path, &temporary)?;
        validate_staged_metadata(path, &metadata, &temporary, &file)?;
        file.sync_all()
            .map_err(|error| AifixError::io_path(temporary.clone(), error))
    })();
    drop(file);
    if let Err(error) = result {
        return Err(with_temporary_cleanup(error, &temporary));
    }
    Ok(temporary)
}

/// Generate an unpredictable source-staging suffix from operating-system
/// entropy so restrictive temporary contents are not discoverable by name.
#[cfg(unix)]
fn secure_temporary_suffix() -> Result<String, AifixError>
{
    let entropy_path = Utf8Path::new("/dev/urandom");
    let mut entropy = [0_u8; 16];
    fs::File::open(entropy_path)
        .and_then(|mut source| source.read_exact(&mut entropy))
        .map_err(|error| AifixError::io_path(entropy_path.to_owned(), error))?;
    Ok(format!("{:032x}", u128::from_ne_bytes(entropy)))
}

#[cfg(not(unix))]
fn secure_temporary_suffix() -> Result<String, AifixError>
{
    Err(AifixError::invalid_argument(
        "secure automatic LSP source staging is unsupported on this platform",
    ))
}

/// Reject target metadata that a rename-based replacement cannot preserve.
#[cfg(unix)]
fn validate_replacement_metadata(
    path: &Utf8Path,
    metadata: &fs::Metadata,
) -> Result<(), AifixError>
{
    if !metadata.is_file() {
        return Err(AifixError::invalid_argument(format!(
            "automatic LSP replacement requires a regular file: `{path}`"
        )));
    }
    if metadata.nlink() != 1 {
        return Err(AifixError::invalid_argument(format!(
            "automatic LSP replacement rejects multiply linked files: `{path}`"
        )));
    }
    #[cfg(target_os = "macos")]
    if std::os::macos::fs::MetadataExt::st_flags(metadata) != 0 {
        return Err(AifixError::invalid_argument(format!(
            "automatic LSP replacement cannot preserve file flags: `{path}`"
        )));
    }
    Ok(())
}

/// Copy and verify ACLs and extended attributes onto one staged replacement.
#[cfg(unix)]
fn copy_security_metadata(
    source: &Utf8Path,
    staged: &Utf8Path,
) -> Result<(), AifixError>
{
    let mut attributes = BTreeMap::new();
    let mut retained = 0_usize;
    for name in
        xattr::list_deref(source).map_err(|error| AifixError::io_path(source.to_owned(), error))?
    {
        let value = xattr::get_deref(source, &name)
            .map_err(|error| AifixError::io_path(source.to_owned(), error))?
            .ok_or_else(|| {
                AifixError::invalid_argument(format!(
                    "extended attribute changed while staging `{source}`"
                ))
            })?;
        retained = retained
            .checked_add(name.as_encoded_bytes().len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| AifixError::process("security metadata byte accounting overflowed"))?;
        if retained > MAX_SECURITY_METADATA_BYTES {
            return Err(AifixError::invalid_argument(format!(
                "source security metadata exceeded {MAX_SECURITY_METADATA_BYTES} bytes: `{source}`"
            )));
        }
        attributes.insert(name, value);
    }
    for (name, value) in &attributes {
        xattr::set_deref(staged, name, value)
            .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
    }
    let acl = exacl::getfacl(source, None)
        .map_err(|error| AifixError::io_path(source.to_owned(), error))?;
    exacl::setfacl(&[staged.as_std_path()], &acl, None)
        .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
    let staged_acl = exacl::getfacl(staged, None)
        .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
    if staged_acl != acl {
        return Err(AifixError::invalid_argument(format!(
            "staged replacement did not preserve the source ACL: `{source}`"
        )));
    }
    let staged_attribute_names = xattr::list_deref(staged)
        .map_err(|error| AifixError::io_path(staged.to_owned(), error))?
        .collect::<BTreeSet<_>>();
    let expected_attribute_names = attributes.keys().cloned().collect::<BTreeSet<_>>();
    let metadata_preserved =
        staging_attributes_preserve_source(&expected_attribute_names, &staged_attribute_names);
    if !metadata_preserved {
        return Err(AifixError::invalid_argument(format!(
            "staged replacement did not preserve the extended-attribute set for `{source}`: \
             expected {expected_attribute_names:?}, actual {staged_attribute_names:?}"
        )));
    }
    for (name, expected) in attributes {
        let actual = xattr::get_deref(staged, &name)
            .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
        if actual.as_deref() != Some(expected.as_slice()) {
            return Err(AifixError::invalid_argument(format!(
                "staged replacement did not preserve extended attributes: `{source}`"
            )));
        }
    }
    Ok(())
}

/// Return whether the operating system may synthesize an attribute on a new
/// replacement file without weakening source-owned metadata checks.
#[cfg(target_os = "macos")]
fn is_os_managed_staging_attribute(name: &std::ffi::OsStr) -> bool
{
    name.as_encoded_bytes() == b"com.apple.provenance"
}

/// Non-macOS platforms have no accepted staging-only extended attributes.
#[cfg(all(unix, not(target_os = "macos")))]
fn is_os_managed_staging_attribute(_name: &std::ffi::OsStr) -> bool
{
    false
}

/// Return whether a staged replacement retains every source attribute and
/// adds at most operating-system-managed staging metadata.
#[cfg(unix)]
fn staging_attributes_preserve_source(
    source: &BTreeSet<std::ffi::OsString>,
    staged: &BTreeSet<std::ffi::OsString>,
) -> bool
{
    source.is_subset(staged)
        && staged
            .difference(source)
            .all(|name| is_os_managed_staging_attribute(name))
}

/// Revalidate the complete ACL and extended-attribute set immediately before
/// replacing a synchronized source.
#[cfg(unix)]
fn validate_security_metadata_equivalent(
    source: &Utf8Path,
    staged: &Utf8Path,
) -> Result<(), AifixError>
{
    let source_names = xattr::list_deref(source)
        .map_err(|error| AifixError::io_path(source.to_owned(), error))?
        .collect::<BTreeSet<_>>();
    let staged_names = xattr::list_deref(staged)
        .map_err(|error| AifixError::io_path(staged.to_owned(), error))?
        .collect::<BTreeSet<_>>();
    let metadata_preserved = staging_attributes_preserve_source(&source_names, &staged_names);
    if !metadata_preserved {
        return Err(AifixError::invalid_argument(format!(
            "source extended attributes changed during replacement: `{source}`"
        )));
    }
    let mut retained = 0_usize;
    for name in source_names {
        let source_value = xattr::get_deref(source, &name)
            .map_err(|error| AifixError::io_path(source.to_owned(), error))?;
        let staged_value = xattr::get_deref(staged, &name)
            .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
        let Some(source_value) = source_value
        else {
            return Err(AifixError::invalid_argument(format!(
                "source extended attributes changed during replacement: `{source}`"
            )));
        };
        retained = retained
            .checked_add(name.as_encoded_bytes().len())
            .and_then(|bytes| bytes.checked_add(source_value.len()))
            .ok_or_else(|| AifixError::process("security metadata byte accounting overflowed"))?;
        if retained > MAX_SECURITY_METADATA_BYTES || staged_value.as_deref() != Some(&source_value)
        {
            return Err(AifixError::invalid_argument(format!(
                "source extended attributes changed during replacement: `{source}`"
            )));
        }
    }
    let source_acl = exacl::getfacl(source, None)
        .map_err(|error| AifixError::io_path(source.to_owned(), error))?;
    let staged_acl = exacl::getfacl(staged, None)
        .map_err(|error| AifixError::io_path(staged.to_owned(), error))?;
    if source_acl != staged_acl {
        return Err(AifixError::invalid_argument(format!(
            "source ACL changed during replacement: `{source}`"
        )));
    }
    Ok(())
}

/// Verify that staged-file ownership and mode match the target before rename.
#[cfg(unix)]
fn validate_staged_metadata(
    path: &Utf8Path,
    target: &fs::Metadata,
    temporary: &Utf8Path,
    staged: &fs::File,
) -> Result<(), AifixError>
{
    let staged_metadata = staged
        .metadata()
        .map_err(|error| AifixError::io_path(temporary.to_owned(), error))?;
    if staged_metadata.uid() != target.uid()
        || staged_metadata.gid() != target.gid()
        || staged_metadata.mode() != target.mode()
    {
        return Err(AifixError::invalid_argument(format!(
            "automatic LSP replacement cannot preserve ownership or mode for `{path}`"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_replacement_metadata(
    path: &Utf8Path,
    _metadata: &fs::Metadata,
) -> Result<(), AifixError>
{
    Err(AifixError::invalid_argument(format!(
        "automatic LSP replacement is unsupported on this platform: `{path}`"
    )))
}

#[cfg(not(unix))]
fn copy_security_metadata(
    _source: &Utf8Path,
    _staged: &Utf8Path,
) -> Result<(), AifixError>
{
    Ok(())
}

#[cfg(not(unix))]
fn validate_security_metadata_equivalent(
    _source: &Utf8Path,
    _staged: &Utf8Path,
) -> Result<(), AifixError>
{
    Ok(())
}

#[cfg(not(unix))]
fn validate_staged_metadata(
    _path: &Utf8Path,
    _target: &fs::Metadata,
    _temporary: &Utf8Path,
    _staged: &fs::File,
) -> Result<(), AifixError>
{
    Ok(())
}

/// Revalidate and atomically replace one target with a staged same-directory
/// file.
fn replace_staged_if_unchanged(
    path: &Utf8Path,
    expected: &str,
    temporary: &Utf8Path,
) -> Result<(), AifixError>
{
    replace_staged_if_unchanged_with(path, expected, temporary, || Ok(()))
}

/// Replace one staged file after an optional last-moment validation hook.
fn replace_staged_if_unchanged_with(
    path: &Utf8Path,
    expected: &str,
    temporary: &Utf8Path,
    before_exchange: impl FnOnce() -> Result<(), AifixError>,
) -> Result<(), AifixError>
{
    let mut preserve_temporary = false;
    let result = (|| {
        if !file_matches_expected(path, expected.as_bytes())? {
            return Err(AifixError::invalid_argument(format!(
                "LSP workspace edit target changed during replacement: `{path}`"
            )));
        }
        let target =
            fs::metadata(path).map_err(|error| AifixError::io_path(path.to_owned(), error))?;
        validate_replacement_metadata(path, &target)?;
        let staged = fs::File::open(temporary)
            .map_err(|error| AifixError::io_path(temporary.to_owned(), error))?;
        validate_staged_metadata(path, &target, temporary, &staged)?;
        validate_security_metadata_equivalent(path, temporary)?;
        before_exchange()?;
        exchange_staged_with_target(temporary, path)?;

        let displaced_result = (|| {
            let displaced = fs::symlink_metadata(temporary)
                .map_err(|error| AifixError::io_path(temporary.to_owned(), error))?;
            validate_replacement_metadata(path, &displaced)?;
            if !file_matches_expected(temporary, expected.as_bytes())? {
                return Err(AifixError::invalid_argument(format!(
                    "LSP workspace edit target changed during atomic exchange: `{path}`"
                )));
            }
            let replacement = fs::File::open(path)
                .map_err(|error| AifixError::io_path(path.to_owned(), error))?;
            validate_staged_metadata(path, &displaced, path, &replacement)?;
            validate_security_metadata_equivalent(temporary, path)
        })();

        if let Err(error) = displaced_result {
            preserve_temporary = true;
            return match exchange_staged_with_target(temporary, path) {
                | Ok(()) => Err(AifixError::process(format!(
                    "{error}; source was restored atomically and the displaced replacement was \
                     retained at `{temporary}`"
                ))),
                | Err(rollback) => Err(AifixError::process(format!(
                    "{error}; atomic source restoration failed: {rollback}; the displaced source \
                     remains at `{temporary}`"
                ))),
            };
        }

        if let Err(cleanup) = fs::remove_file(temporary) {
            preserve_temporary = true;
            let error = AifixError::io_path(temporary.to_owned(), cleanup);
            return match exchange_staged_with_target(temporary, path) {
                | Ok(()) => Err(with_temporary_cleanup(error, temporary)),
                | Err(rollback) => Err(AifixError::process(format!(
                    "{error}; atomic source restoration failed: {rollback}; the displaced source \
                     remains at `{temporary}`"
                ))),
            };
        }
        Ok(())
    })();

    result.map_err(|error| {
        if preserve_temporary {
            error
        }
        else {
            with_temporary_cleanup(error, temporary)
        }
    })
}
/// Atomically exchange a staged file with its target without discarding either
/// inode.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange_staged_with_target(
    staged: &Utf8Path,
    target: &Utf8Path,
) -> Result<(), AifixError>
{
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staged.as_std_path(),
        rustix::fs::CWD,
        target.as_std_path(),
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|error| {
        AifixError::process(format!(
            "failed to atomically exchange staged LSP source `{staged}` with `{target}`: {error}"
        ))
    })
}

/// Reject replacement where the platform has no atomic exchange primitive.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange_staged_with_target(
    staged: &Utf8Path,
    target: &Utf8Path,
) -> Result<(), AifixError>
{
    Err(AifixError::invalid_argument(format!(
        "automatic LSP replacement requires atomic file exchange support: `{staged}` and \
         `{target}`"
    )))
}

/// Preserve a primary failure while reporting a leaked staged source copy.
fn with_temporary_cleanup(
    error: AifixError,
    temporary: &Utf8Path,
) -> AifixError
{
    match fs::remove_file(temporary) {
        | Ok(()) => error,
        | Err(cleanup) => AifixError::process(format!(
            "{error}; failed to remove staged LSP source copy `{temporary}`: {cleanup}"
        )),
    }
}

/// Remove every uncommitted temporary file while preserving a primary error.
fn with_staged_cleanup(
    error: AifixError,
    changes: &mut [PreparedWorkspaceEdit],
) -> AifixError
{
    let failures = changes
        .iter_mut()
        .filter_map(|change| change.temporary.take())
        .filter_map(|temporary| {
            fs::remove_file(&temporary)
                .err()
                .map(|cleanup| format!("{temporary}: {cleanup}"))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        error
    }
    else {
        AifixError::process(format!(
            "{error}; failed to remove staged LSP source copies: {}",
            failures.join("; ")
        ))
    }
}

/// Restore every already replaced file, preserving the primary failure when
/// possible.
fn with_workspace_rollback(
    error: AifixError,
    committed: &[PreparedWorkspaceEdit],
) -> AifixError
{
    let failures = committed
        .iter()
        .rev()
        .filter_map(|change| {
            atomic_replace_if_unchanged(&change.path, &change.updated, &change.expected)
                .err()
                .map(|rollback| format!("{}: {rollback}", change.path))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        error
    }
    else {
        AifixError::process(format!(
            "{error}; LSP workspace rollback also failed: {}",
            failures.join("; ")
        ))
    }
}

/// Read one UTF-8 file without retaining more than the supplied byte bound.
fn read_utf8_file_bounded(
    path: &Utf8Path,
    max_bytes: usize,
    label: &str,
) -> Result<String, AifixError>
{
    let bytes = read_file_bytes_bounded(path, max_bytes, label)?;
    String::from_utf8(bytes)
        .map_err(|error| AifixError::utf8(format!("{label} was not UTF-8: {error}")))
}

/// Read one file as bytes without retaining more than the supplied bound.
fn read_file_bytes_bounded(
    path: &Utf8Path,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>, AifixError>
{
    let read_limit = max_bytes
        .checked_add(1)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| AifixError::process(format!("{label} byte limit overflowed")))?;
    let file = fs::File::open(path).map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    if bytes.len() > max_bytes {
        return Err(AifixError::process(format!(
            "{label} exceeded {max_bytes} bytes: `{path}`"
        )));
    }
    Ok(bytes)
}

/// Compare one source with synchronized bytes while reading at most one extra
/// byte, so concurrent growth is reported as a mismatch rather than retained.
fn file_matches_expected(
    path: &Utf8Path,
    expected: &[u8],
) -> Result<bool, AifixError>
{
    let read_limit = expected
        .len()
        .checked_add(1)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| AifixError::process("LSP synchronized source byte limit overflowed"))?;
    let file = fs::File::open(path).map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    let mut actual = Vec::with_capacity(expected.len().saturating_add(1));
    file.take(read_limit)
        .read_to_end(&mut actual)
        .map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    Ok(actual == expected)
}

/// Estimate heap bytes retained by one URI-keyed diagnostic publication.
fn diagnostic_entry_retained_bytes(
    uri: &str,
    diagnostics: &[Value],
) -> Result<usize, AifixError>
{
    let mut total = MAP_ENTRY_OVERHEAD_BYTES
        .checked_add(uri.len())
        .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
    for diagnostic in diagnostics {
        total = total
            .checked_add(json_value_retained_bytes(diagnostic)?)
            .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))?;
    }
    Ok(total)
}

/// Estimate bytes retained by one actionable publication and its version key.
fn actionable_entry_retained_bytes(
    uri: &str,
    diagnostics: &[Value],
) -> Result<usize, AifixError>
{
    diagnostic_entry_retained_bytes(uri, diagnostics)?
        .checked_add(uri_version_entry_retained_bytes(uri)?)
        .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))
}

/// Estimate bytes retained by one URI-to-version map entry.
fn uri_version_entry_retained_bytes(uri: &str) -> Result<usize, AifixError>
{
    MAP_ENTRY_OVERHEAD_BYTES
        .checked_add(uri.len())
        .ok_or_else(|| AifixError::process("LSP diagnostic byte accounting overflowed"))
}

/// Conservatively estimate bytes retained by one decoded JSON value.
fn json_value_retained_bytes(value: &Value) -> Result<usize, AifixError>
{
    let mut total = JSON_VALUE_OVERHEAD_BYTES;
    match *value {
        | Value::Null => {},
        | Value::Bool(_) | Value::Number(_) => {
            total = total
                .checked_add(core::mem::size_of::<Value>())
                .ok_or_else(|| AifixError::process("LSP JSON byte accounting overflowed"))?;
        },
        | Value::String(ref text) => {
            total = total
                .checked_add(text.len())
                .ok_or_else(|| AifixError::process("LSP JSON byte accounting overflowed"))?;
        },
        | Value::Array(ref values) => {
            for item in values {
                total = total
                    .checked_add(json_value_retained_bytes(item)?)
                    .ok_or_else(|| AifixError::process("LSP JSON byte accounting overflowed"))?;
            }
        },
        | Value::Object(ref fields) => {
            for field in fields {
                total = total
                    .checked_add(MAP_ENTRY_OVERHEAD_BYTES)
                    .and_then(|bytes| bytes.checked_add(field.0.len()))
                    .ok_or_else(|| AifixError::process("LSP JSON byte accounting overflowed"))?;
                total = total
                    .checked_add(json_value_retained_bytes(field.1)?)
                    .ok_or_else(|| AifixError::process("LSP JSON byte accounting overflowed"))?;
            }
        },
    }
    Ok(total)
}

/// Estimate retained bytes for one reader-thread event.
fn reader_event_retained_bytes(event: &ReaderEvent) -> Result<usize, AifixError>
{
    event.as_ref().map_or_else(
        |error| {
            error
                .len()
                .checked_add(128)
                .ok_or_else(|| AifixError::process("LSP reader-event byte accounting overflowed"))
        },
        json_value_retained_bytes,
    )
}
/// Replace one retained-state byte contribution with checked accounting.
fn replace_retained_bytes(
    current: usize,
    removed: usize,
    added: usize,
    label: &str,
) -> Result<usize, AifixError>
{
    current
        .checked_sub(removed)
        .and_then(|remaining| remaining.checked_add(added))
        .ok_or_else(|| AifixError::process(format!("{label} byte accounting overflowed")))
}

/// Estimate retained bytes for one synchronized source document.
fn document_entry_retained_bytes(
    uri: &str,
    path: &Utf8Path,
    text_bytes: usize,
) -> Result<usize, AifixError>
{
    MAP_ENTRY_OVERHEAD_BYTES
        .checked_add(uri.len())
        .and_then(|bytes| bytes.checked_add(path.as_str().len()))
        .and_then(|bytes| bytes.checked_add(text_bytes))
        .ok_or_else(|| AifixError::process("LSP document byte accounting overflowed"))
}

/// Discover source documents deterministically without following symlinks.
fn discover_source_files(
    root: &Utf8Path,
    extensions: &[String],
    deadline: Instant,
) -> Result<Vec<Utf8PathBuf>, AifixError>
{
    let mut pending = vec![root.to_owned()];
    let mut sources = Vec::new();
    let mut visited_entries = 0_usize;
    let mut discovered_directories = 1_usize;
    let mut path_bytes = root.as_str().len();
    while let Some(directory) = pending.pop() {
        if Instant::now() >= deadline {
            return Err(AifixError::process(
                "LSP source discovery exceeded the complete-session deadline",
            ));
        }
        let entries = fs::read_dir(&directory)
            .map_err(|error| AifixError::io_path(directory.clone(), error))?;
        let mut paths = Vec::new();
        for entry in entries {
            if Instant::now() >= deadline {
                return Err(AifixError::process(
                    "LSP source discovery exceeded the complete-session deadline",
                ));
            }
            visited_entries = visited_entries
                .checked_add(1)
                .ok_or_else(|| AifixError::process("LSP source discovery count overflowed"))?;
            if visited_entries > MAX_DISCOVERY_ENTRIES {
                return Err(AifixError::process(format!(
                    "LSP source discovery exceeded {MAX_DISCOVERY_ENTRIES} entries"
                )));
            }
            if paths.len() >= MAX_DIRECTORY_ENTRIES {
                return Err(AifixError::process(format!(
                    "LSP source discovery exceeded {MAX_DIRECTORY_ENTRIES} entries in \
                     `{directory}`"
                )));
            }
            let entry = entry.map_err(|error| AifixError::io_path(directory.clone(), error))?;
            let path = entry.path();
            path_bytes = path_bytes
                .checked_add(path.as_os_str().as_encoded_bytes().len())
                .ok_or_else(|| AifixError::process("LSP source path byte count overflowed"))?;
            if path_bytes > MAX_DISCOVERY_PATH_BYTES {
                return Err(AifixError::process(format!(
                    "LSP source discovery exceeded {MAX_DISCOVERY_PATH_BYTES} path bytes"
                )));
            }
            if let Ok(path) = Utf8PathBuf::from_path_buf(path) {
                paths.push(path);
            }
        }
        paths.sort();
        paths.reverse();
        for path in paths {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| AifixError::io_path(path.clone(), error))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if !is_excluded_directory(&path) {
                    discovered_directories =
                        discovered_directories.checked_add(1).ok_or_else(|| {
                            AifixError::process("LSP source directory count overflowed")
                        })?;
                    if discovered_directories > MAX_DISCOVERY_DIRECTORIES {
                        return Err(AifixError::process(format!(
                            "LSP source discovery exceeded {MAX_DISCOVERY_DIRECTORIES} directories"
                        )));
                    }
                    pending.push(path);
                }
                continue;
            }
            if metadata.is_file()
                && path.extension().is_some_and(|extension| {
                    extensions.iter().any(|candidate| candidate == extension)
                })
            {
                sources.push(path);
                if sources.len() > MAX_SOURCE_FILES {
                    return Err(AifixError::invalid_argument(format!(
                        "LSP code-action source discovery exceeded {MAX_SOURCE_FILES} files"
                    )));
                }
            }
        }
    }
    sources.sort();
    Ok(sources)
}

/// Return whether a recursive source directory is known generated state.
fn is_excluded_directory(path: &Utf8Path) -> bool
{
    matches!(
        path.file_name(),
        Some(".git" | ".jj" | ".beads" | "target" | "node_modules")
    )
}

/// Convert a path to its canonical UTF-8 representation.
fn canonical_utf8(path: &Utf8Path) -> Result<Utf8PathBuf, AifixError>
{
    let canonical =
        fs::canonicalize(path).map_err(|error| AifixError::io_path(path.to_owned(), error))?;
    Utf8PathBuf::from_path_buf(canonical).map_err(|invalid| {
        AifixError::utf8(format!("path was not valid UTF-8: {}", invalid.display()))
    })
}

/// Encode one absolute path as an RFC 8089 file URI.
fn file_uri(path: &Utf8Path) -> String
{
    file_uri_from_path_text(path.as_str(), cfg!(windows))
}

/// Encode one UTF-8 path using platform-specific file URI rules.
fn file_uri_from_path_text(
    path: &str,
    windows: bool,
) -> String
{
    let normalized = if windows {
        path.replace('\\', "/")
    }
    else {
        path.to_owned()
    };
    let mut uri = if windows && normalized.starts_with("//") {
        String::from("file:")
    }
    else if windows {
        String::from("file:///")
    }
    else {
        String::from("file://")
    };
    let encoded_path = if windows && !normalized.starts_with("//") {
        normalized.trim_start_matches('/')
    }
    else {
        normalized.as_str()
    };
    for byte in encoded_path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        }
        else {
            uri.push('%');
            uri.push(hex_digit(byte >> 4));
            uri.push(hex_digit(byte & 0x0F));
        }
    }
    uri
}

/// Decode one local file URI into a UTF-8 path.
fn path_from_file_uri(uri: &str) -> Result<Utf8PathBuf, AifixError>
{
    file_path_text_from_uri(uri, cfg!(windows)).map(Utf8PathBuf::from)
}

/// Decode an RFC 8089 file URI using platform-specific path rules.
fn file_path_text_from_uri(
    uri: &str,
    windows: bool,
) -> Result<String, AifixError>
{
    let encoded = uri.strip_prefix("file://").ok_or_else(|| {
        AifixError::invalid_argument(format!("LSP workspace edit used non-file URI `{uri}`"))
    })?;
    if !windows && !encoded.starts_with('/') {
        return Err(AifixError::invalid_argument(format!(
            "LSP workspace edit used a file URI authority: `{uri}`"
        )));
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0_usize;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let high = bytes
                .get(index + 1)
                .copied()
                .and_then(hex_value)
                .ok_or_else(|| {
                    AifixError::invalid_argument(format!(
                        "LSP file URI contained invalid percent encoding: `{uri}`"
                    ))
                })?;
            let low = bytes
                .get(index + 2)
                .copied()
                .and_then(hex_value)
                .ok_or_else(|| {
                    AifixError::invalid_argument(format!(
                        "LSP file URI contained invalid percent encoding: `{uri}`"
                    ))
                })?;
            decoded.push((high << 4_u32) | low);
            index = index.saturating_add(3);
        }
        else {
            decoded.push(byte);
            index = index.saturating_add(1);
        }
    }
    let mut path = String::from_utf8(decoded)
        .map_err(|error| AifixError::utf8(format!("LSP file URI was not UTF-8: {error}")))?;
    if windows {
        let drive_path = path.strip_prefix('/').is_some_and(is_windows_drive_path);
        if drive_path {
            path.remove(0);
        }
        else if !path.starts_with('/') {
            path.insert_str(0, "//");
        }
        path = path.replace('/', "\\");
    }
    Ok(path)
}

/// Return whether a path begins with an ASCII Windows drive prefix.
fn is_windows_drive_path(path: &str) -> bool
{
    let bytes = path.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':')
}

/// Encode one four-bit value as an uppercase hexadecimal digit.
///
/// Values outside one nibble violate an internal caller invariant; debug
/// builds assert while release builds still return a deterministic character.
fn hex_digit(value: u8) -> char
{
    debug_assert!(value < 16, "hex digit exceeded one nibble");
    let digit = if value < 10 {
        b'0' + value
    }
    else {
        b'A' + value.saturating_sub(10)
    };
    char::from(digit)
}

/// Decode one hexadecimal ASCII digit.
fn hex_value(byte: u8) -> Option<u8>
{
    match byte {
        | b'0' ..= b'9' => Some(byte - b'0'),
        | b'a' ..= b'f' => Some(byte - b'a' + 10),
        | b'A' ..= b'F' => Some(byte - b'A' + 10),
        | _ => None,
    }
}

/// Drain supervised writer requests through the child stdin pipe.
fn write_messages(
    input: ChildStdin,
    requests: &Receiver<WriteRequest>,
)
{
    let mut input = BufWriter::new(input);
    while let Ok(request) = requests.recv() {
        let result = write!(input, "Content-Length: {}\r\n\r\n", request.payload.len())
            .and_then(|()| input.write_all(&request.payload))
            .and_then(|()| input.flush())
            .map_err(|error| error.to_string());
        let failed = result.is_err();
        if request.result.send(result).is_err() || failed {
            return;
        }
    }
}

/// Read framed LSP messages until EOF and forward typed events.
fn read_messages(
    output: impl io::Read,
    sender: &SyncSender<ReaderEvent>,
)
{
    let mut reader = BufReader::new(output);
    loop {
        match read_message(&mut reader) {
            | Ok(Some(message)) => {
                if sender.send(Ok(message)).is_err() {
                    return;
                }
            },
            | Ok(None) => return,
            | Err(error) => {
                drop(sender.send(Err(error)));
                return;
            },
        }
    }
}

/// Read one Content-Length-framed JSON-RPC message.
fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, String>
{
    let mut content_length = None;
    let mut content_type_seen = false;
    let mut header_bytes = 0_usize;
    loop {
        let Some(header) = read_bounded_header_line(reader, &mut header_bytes)?
        else {
            return if content_length.is_none() && header_bytes == 0 {
                Ok(None)
            }
            else {
                Err("LSP stream ended inside message headers".to_owned())
            };
        };
        if header == b"\r\n" || header == b"\n" {
            break;
        }
        let header = str::from_utf8(&header)
            .map_err(|error| format!("LSP header was not UTF-8: {error}"))?
            .trim_end();
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| "LSP header had no name/value separator".to_owned())?;
        let value = value.trim();
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err("LSP message contained duplicate Content-Length headers".to_owned());
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|error| format!("invalid LSP Content-Length: {error}"))?,
            );
        }
        else if name.trim().eq_ignore_ascii_case("Content-Type") {
            if content_type_seen {
                return Err("LSP message contained duplicate Content-Type headers".to_owned());
            }
            validate_lsp_content_type(value)?;
            content_type_seen = true;
        }
    }
    let content_length =
        content_length.ok_or_else(|| "LSP message had no Content-Length".to_owned())?;
    if content_length > MAX_LSP_MESSAGE_BYTES {
        return Err(format!(
            "LSP message exceeded {MAX_LSP_MESSAGE_BYTES} bytes"
        ));
    }
    let mut payload = vec![0_u8; content_length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("failed to read LSP payload: {error}"))?;
    let value: Value = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    let retained = json_value_retained_bytes(&value).map_err(|error| error.to_string())?;
    if retained > MAX_DEFERRED_LSP_BYTES {
        return Err(format!(
            "decoded LSP message exceeded {MAX_DEFERRED_LSP_BYTES} retained bytes"
        ));
    }
    Ok(Some(value))
}

/// Validate the optional LSP Content-Type and its UTF-8 charset.
fn validate_lsp_content_type(value: &str) -> Result<(), String>
{
    let mut parts = value.split(';');
    let media_type = parts.next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/vscode-jsonrpc") {
        return Err(format!("unsupported LSP Content-Type `{value}`"));
    }
    let mut charset_seen = false;
    for parameter in parts {
        let (name, charset) = parameter
            .split_once('=')
            .ok_or_else(|| format!("malformed LSP Content-Type parameter `{parameter}`"))?;
        if !name.trim().eq_ignore_ascii_case("charset") || charset_seen {
            return Err(format!(
                "unsupported LSP Content-Type parameter `{parameter}`"
            ));
        }
        let charset = charset.trim().trim_matches('"');
        if !charset.eq_ignore_ascii_case("utf-8") && !charset.eq_ignore_ascii_case("utf8") {
            return Err(format!("unsupported LSP Content-Type charset `{charset}`"));
        }
        charset_seen = true;
    }
    Ok(())
}

/// Read one header line while enforcing the aggregate framing-header bound.
fn read_bounded_header_line(
    reader: &mut impl BufRead,
    total: &mut usize,
) -> Result<Option<Vec<u8>>, String>
{
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("failed to read LSP header: {error}"))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            }
            else {
                Err("LSP stream ended inside a header line".to_owned())
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index.saturating_add(1));
        let next_total = total
            .checked_add(take)
            .ok_or_else(|| "LSP header byte count overflowed".to_owned())?;
        if next_total > MAX_LSP_HEADER_BYTES {
            return Err(format!(
                "LSP message headers exceeded {MAX_LSP_HEADER_BYTES} bytes"
            ));
        }
        let retained = available
            .get(.. take)
            .ok_or_else(|| "LSP header slice exceeded its read buffer".to_owned())?;
        line.extend_from_slice(retained);
        reader.consume(take);
        *total = next_total;
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

/// Drain server stderr while retaining only a bounded prefix for failures.
fn drain_stderr(
    stderr: impl io::Read,
    target: &Arc<Mutex<Vec<u8>>>,
)
{
    let mut stderr = BufReader::new(stderr);
    let mut buffer = [0_u8; 4096];
    loop {
        let Ok(bytes) = stderr.read(&mut buffer)
        else {
            return;
        };
        if bytes == 0 {
            return;
        }
        let Ok(mut captured) = target.lock()
        else {
            return;
        };
        let remaining = MAX_LSP_STDERR_BYTES.saturating_sub(captured.len());
        let retained = bytes.min(remaining);
        let Some(retained_bytes) = buffer.get(.. retained)
        else {
            debug_assert!(false, "retained stderr prefix exceeded read buffer");
            return;
        };
        captured.extend_from_slice(retained_bytes);
    }
}

#[cfg(test)]
mod tests
{
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// Verifies arbitrary staging attributes cannot pass metadata validation.
    #[cfg(unix)]
    #[test]
    fn staging_attribute_validation_rejects_unowned_metadata()
    {
        let source = BTreeSet::new();
        let staged = BTreeSet::from([std::ffi::OsString::from("user.unexpected")]);
        assert!(!staging_attributes_preserve_source(&source, &staged));
    }

    /// Verifies macOS-managed provenance can accompany an otherwise exact
    /// source attribute set.
    #[cfg(target_os = "macos")]
    #[test]
    fn staging_attribute_validation_accepts_macos_provenance()
    {
        let source = BTreeSet::new();
        let staged = BTreeSet::from([std::ffi::OsString::from("com.apple.provenance")]);
        assert!(staging_attributes_preserve_source(&source, &staged));
    }

    /// Verifies LSP positions count UTF-16 code units, not UTF-8 bytes.
    #[test]
    fn position_offset_handles_utf16_surrogate_pairs()
    {
        let text = "a😀b\n";
        let deadline = Instant::now() + Duration::from_secs(1);
        let offsets =
            resolve_position_offsets(text, &[(0, 3)], deadline).expect("valid UTF-16 position");
        assert_eq!(offsets.get(&(0, 3)), Some(&5));
        assert!(resolve_position_offsets(text, &[(0, 2)], deadline).is_err());
    }

    /// Verifies edits apply from the end and reject overlap.
    #[test]
    fn text_edits_apply_in_reverse_order_and_reject_overlap()
    {
        let edits = vec![
            json!({ "range": { "start": { "line": 0_u64, "character": 0_u64 }, "end": { "line": 0_u64, "character": 1_u64 } }, "newText": "A" }),
            json!({ "range": { "start": { "line": 0_u64, "character": 2_u64 }, "end": { "line": 0_u64, "character": 3_u64 } }, "newText": "C" }),
        ];
        assert_eq!(
            apply_text_edits("abc", &edits, Instant::now() + Duration::from_secs(1))
                .expect("non-overlapping edits should apply"),
            "AbC"
        );
        let overlap = vec![
            json!({ "range": { "start": { "line": 0_u64, "character": 0_u64 }, "end": { "line": 0_u64, "character": 2_u64 } }, "newText": "x" }),
            json!({ "range": { "start": { "line": 0_u64, "character": 1_u64 }, "end": { "line": 0_u64, "character": 3_u64 } }, "newText": "y" }),
        ];
        assert!(
            apply_text_edits("abc", &overlap, Instant::now() + Duration::from_secs(1)).is_err()
        );
    }

    /// Verifies file URI encoding round-trips UTF-8 and spaces.
    #[test]
    fn file_uri_round_trips_utf8_paths()
    {
        let path = Utf8Path::new("/tmp/aifix λ/file name.rs");
        let uri = file_uri(path);
        assert_eq!(
            path_from_file_uri(&uri).expect("encoded file URI should decode"),
            path
        );
    }

    /// Verifies arbitrary profiles require explicit server and document data.
    #[test]
    fn custom_code_action_config_requires_complete_contract()
    {
        let profile = ProfileConfig {
            code_actions: Some(crate::config::CodeActionConfig::default()),
            ..ProfileConfig::default()
        };
        let error = resolve_code_action_config("custom-profile", Some(&profile))
            .expect_err("incomplete custom code-action config should fail");
        assert!(error.to_string().contains("code_actions.argv"));
    }
    /// Verifies distinct findings at one range do not collide in loop
    /// detection or action correlation.
    #[test]
    fn diagnostic_key_includes_source_and_message()
    {
        let first = json!({
            "range": {
                "start": { "line": 0_u64, "character": 0_u64 },
                "end": { "line": 0_u64, "character": 1_u64 }
            },
            "code": "E1",
            "source": "first",
            "message": "first message"
        });
        let second = json!({
            "range": first["range"],
            "code": "E1",
            "source": "second",
            "message": "second message"
        });
        assert_ne!(
            DiagnosticKey::from_diagnostic(&first),
            DiagnosticKey::from_diagnostic(&second)
        );
    }

    /// Verifies numeric and options-form synchronization capabilities resolve
    /// only to supported full or incremental modes.
    #[test]
    fn text_sync_capability_resolution_is_explicit()
    {
        assert_eq!(
            resolve_text_document_sync(&json!({ "capabilities": { "textDocumentSync": 1_u64 } }))
                .expect("full synchronization should resolve"),
            TextDocumentSync::Full
        );
        assert_eq!(
            resolve_text_document_sync(
                &json!({ "capabilities": { "textDocumentSync": { "openClose": true, "change": 2_u64 } } })
            )
            .expect("incremental synchronization should resolve"),
            TextDocumentSync::Incremental
        );
        assert!(
            resolve_text_document_sync(&json!({
                "capabilities": { "textDocumentSync": { "openClose": true, "change": 0_u64 } }
            }))
            .is_err()
        );
        assert!(
            resolve_text_document_sync(&json!({
                "capabilities": {
                    "textDocumentSync": { "openClose": false, "change": 2_u64 }
                }
            }))
            .is_err()
        );
    }

    /// Verifies framing rejects unbounded headers before allocating a payload.
    #[test]
    fn read_message_rejects_oversized_headers()
    {
        let frame = format!(
            "X-Aifix: {}\r\nContent-Length: 2\r\n\r\n{{}}",
            "x".repeat(MAX_LSP_HEADER_BYTES)
        );
        let error = read_message(&mut io::Cursor::new(frame))
            .expect_err("oversized LSP headers should fail");
        assert!(error.contains("headers exceeded"));
    }

    /// Verifies automatic edit scope rejects mixed representations, duplicate
    /// document entries, and confirmation-required annotations.
    #[test]
    fn workspace_edit_scope_rejects_ambiguous_shapes()
    {
        let text_edit = json!({
            "range": {
                "start": { "line": 0_u64, "character": 0_u64 },
                "end": { "line": 0_u64, "character": 1_u64 }
            },
            "newText": "x"
        });
        let mixed = json!({
            "changes": { "file:///a": [text_edit] },
            "documentChanges": [{
                "textDocument": { "uri": "file:///a", "version": 0_i64 },
                "edits": [text_edit]
            }]
        });
        assert!(collect_workspace_edits(&mixed).is_err());

        let duplicate = json!({
            "documentChanges": [
                {
                    "textDocument": { "uri": "file:///a", "version": 0_i64 },
                    "edits": [text_edit]
                },
                {
                    "textDocument": { "uri": "file:///a", "version": 0_i64 },
                    "edits": [text_edit]
                }
            ]
        });
        assert!(collect_workspace_edits(&duplicate).is_err());

        let annotated = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///a", "version": 0_i64 },
                "edits": [{
                    "range": text_edit["range"],
                    "newText": "x",
                    "annotationId": "confirm"
                }]
            }],
            "changeAnnotations": {
                "confirm": { "label": "Confirm", "needsConfirmation": true }
            }
        });
        assert!(collect_workspace_edits(&annotated).is_err());
    }

    /// Verifies a failed staged write leaves the synchronized source intact and
    /// removes its partial temporary file.
    #[test]
    fn atomic_replacement_preserves_source_after_partial_write_failure()
    {
        let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = Utf8PathBuf::from_path_buf(
            std::env::temp_dir().join(format!("aifix-lsp-atomic-{}-{suffix}", std::process::id())),
        )
        .expect("temporary test path should be UTF-8");
        fs::create_dir_all(&directory).expect("temporary test directory should be created");
        let target = directory.join("source.rs");
        fs::write(&target, "ORIGINAL\n").expect("test source should be written");

        let result = atomic_replace_with(
            &target,
            "ORIGINAL\n",
            "REPLACEMENT\n",
            |file, _replacement| {
                file.write_all(b"PARTIAL")?;
                Err(io::Error::other("injected staged-write failure"))
            },
        );
        let source = fs::read_to_string(&target).expect("test source should remain readable");
        let staged_files = fs::read_dir(&directory)
            .expect("temporary directory should remain readable")
            .filter_map(Result::ok)
            .count();
        drop(fs::remove_dir_all(&directory));

        assert!(result.is_err());
        assert_eq!(source, "ORIGINAL\n");
        assert_eq!(staged_files, 1);
    }
    /// Verifies an atomic exchange detects and restores a save that lands
    /// after prevalidation without discarding either version.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn atomic_replacement_preserves_concurrent_save_in_validation_gap()
    {
        let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = Utf8PathBuf::from_path_buf(std::env::temp_dir().join(format!(
            "aifix-lsp-exchange-{}-{suffix}",
            std::process::id()
        )))
        .expect("temporary test path should be UTF-8");
        fs::create_dir_all(&directory).expect("temporary test directory should be created");
        let target = directory.join("source.rs");
        fs::write(&target, "ORIGINAL\n").expect("test source should be written");
        let temporary = stage_atomic_replacement(&target, "ORIGINAL\n", "REPLACEMENT\n")
            .expect("replacement should be staged");

        let result = replace_staged_if_unchanged_with(&target, "ORIGINAL\n", &temporary, || {
            fs::write(&target, "CONCURRENT\n")
                .map_err(|error| AifixError::io_path(target.clone(), error))
        });
        let source = fs::read_to_string(&target).expect("concurrent source should remain readable");
        let retained =
            fs::read_to_string(&temporary).expect("staged replacement should remain retained");
        drop(fs::remove_dir_all(&directory));

        assert!(result.is_err());
        assert_eq!(source, "CONCURRENT\n");
        assert_eq!(retained, "REPLACEMENT\n");
    }

    /// Verifies successful replacement preserves mode and the exact xattr set.
    #[cfg(unix)]
    #[test]
    fn atomic_replacement_preserves_mode_and_extended_attributes()
    {
        let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = Utf8PathBuf::from_path_buf(std::env::temp_dir().join(format!(
            "aifix-lsp-metadata-{}-{suffix}",
            std::process::id()
        )))
        .expect("temporary test path should be UTF-8");
        fs::create_dir_all(&directory).expect("temporary test directory should be created");
        let target = directory.join("source.rs");
        fs::write(&target, "ORIGINAL\n").expect("test source should be written");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))
            .expect("test source permissions should be configured");
        #[cfg(target_os = "macos")]
        let attribute = "com.aifix.test";
        #[cfg(not(target_os = "macos"))]
        let attribute = "user.aifix.test";
        xattr::set_deref(&target, attribute, b"preserved")
            .expect("test source xattr should be configured");

        atomic_replace_if_unchanged(&target, "ORIGINAL\n", "REPLACEMENT\n")
            .expect("metadata-preserving replacement should succeed");
        let source = fs::read_to_string(&target).expect("replacement should remain readable");
        let mode = fs::metadata(&target)
            .expect("replacement metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        let value =
            xattr::get_deref(&target, attribute).expect("replacement xattr should be readable");
        drop(fs::remove_dir_all(&directory));

        assert_eq!(source, "REPLACEMENT\n");
        assert_eq!(mode, 0o640);
        assert_eq!(value.as_deref(), Some(b"preserved".as_slice()));
    }

    /// Verifies a multiply linked source is rejected before any replacement.
    #[cfg(unix)]
    #[test]
    fn atomic_replacement_rejects_hard_linked_sources()
    {
        let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = Utf8PathBuf::from_path_buf(std::env::temp_dir().join(format!(
            "aifix-lsp-hardlink-{}-{suffix}",
            std::process::id()
        )))
        .expect("temporary test path should be UTF-8");
        fs::create_dir_all(&directory).expect("temporary test directory should be created");
        let target = directory.join("source.rs");
        let link = directory.join("source-link.rs");
        fs::write(&target, "ORIGINAL\n").expect("test source should be written");
        fs::hard_link(&target, &link).expect("test hard link should be created");

        let result = atomic_replace_if_unchanged(&target, "ORIGINAL\n", "REPLACEMENT\n");
        let source = fs::read_to_string(&target).expect("test source should remain readable");
        let linked = fs::read_to_string(&link).expect("test hard link should remain readable");
        drop(fs::remove_dir_all(&directory));

        assert!(result.is_err());
        assert_eq!(source, "ORIGINAL\n");
        assert_eq!(linked, "ORIGINAL\n");
    }
}
