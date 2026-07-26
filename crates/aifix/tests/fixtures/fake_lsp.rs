use std::env;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::io::{
    self,
};

fn main()
{
    if let Err(error) = run() {
        eprintln!("fake LSP failure: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String>
{
    let mode = env::args()
        .find_map(|argument| argument.strip_prefix("--mode=").map(str::to_owned))
        .unwrap_or_else(|| "edit".to_owned());
    let stdin = io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut uri = String::new();
    let mut text = String::new();
    let mut uris = Vec::new();
    let mut documents = std::collections::BTreeMap::new();
    let mut content_modified_sent = false;
    let mut same_data_requests = 0_usize;
    let mut version = 0_i64;

    while let Some(message) = read_message(&mut input)? {
        let method = extract_string(&message, "method");
        match method.as_deref() {
            | Some("initialize") => {
                let sync_kind = if mode == "incremental" {
                    "2".to_owned()
                }
                else if mode == "open-close-disabled" {
                    "{\"openClose\":false,\"change\":1}".to_owned()
                }
                else {
                    "1".to_owned()
                };
                let code_action_provider = if mode == "no-code-actions" {
                    "false".to_owned()
                }
                else if mode == "boolean-code-actions" {
                    "true".to_owned()
                }
                else {
                    format!("{{\"resolveProvider\":{}}}", mode == "resolve-escalate")
                };
                let execute_commands = if mode == "command-unadvertised" {
                    "[]"
                }
                else {
                    "[\"fake.apply\"]"
                };
                respond(
                    &mut output,
                    extract_id(&message)?,
                    &format!(
                        "{{\"capabilities\":{{\"codeActionProvider\":{code_action_provider},\
                         \"executeCommandProvider\":{{\"commands\":{execute_commands}}},\
                         \"textDocumentSync\":{sync_kind}}}}}"
                    ),
                )?;
            },
            | Some("initialized") => {},
            | Some("textDocument/didOpen") | Some("textDocument/didChange") => {
                uri = extract_string(&message, "uri")
                    .ok_or_else(|| "document notification had no URI".to_owned())?;
                if method.as_deref() == Some("textDocument/didOpen") && !uris.contains(&uri) {
                    uris.push(uri.clone());
                }
                text = extract_string(&message, "text")
                    .ok_or_else(|| "document notification had no text".to_owned())?;
                version = extract_i64(&message, "version")
                    .ok_or_else(|| "document notification had no version".to_owned())?;
                documents.insert(uri.clone(), text.clone());
                publish_diagnostics(&mut output, &mode, &uri, &text, version)?;
                if mode == "blocked-stdin" {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    return Ok(());
                }
            },
            | Some("textDocument/codeAction") => {
                let id = extract_id(&message)?;
                uri = extract_string(&message, "uri")
                    .ok_or_else(|| "code-action request had no URI".to_owned())?;
                text = documents.get(&uri).cloned().ok_or_else(|| {
                    "code-action request targeted an unopened document".to_owned()
                })?;
                let code = extract_string(&message, "code").unwrap_or_default();
                if mode == "wrong-jsonrpc-version" {
                    write_message(
                        &mut output,
                        &format!("{{\"jsonrpc\":\"1.0\",\"id\":{id},\"result\":[]}}"),
                    )?;
                    continue;
                }
                if mode == "missing-jsonrpc-version" {
                    write_message(&mut output, &format!("{{\"id\":{id},\"result\":[]}}"))?;
                    continue;
                }
                if mode == "array-jsonrpc-root" {
                    write_message(&mut output, "[]")?;
                    continue;
                }
                if mode == "malformed-response" {
                    write_message(&mut output, &format!("{{\"jsonrpc\":\"2.0\",\"id\":{id}}}"))?;
                    continue;
                }
                if mode == "malformed-request-id" {
                    write_message(
                        &mut output,
                        "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"workspace/workspaceFolders\"}",
                    )?;
                    respond(&mut output, id, "[]")?;
                    continue;
                }
                if mode == "registration-request" {
                    request(
                        &mut output,
                        902,
                        "client/registerCapability",
                        "{\"registrations\":[]}",
                    )?;
                    respond(&mut output, id, "[]")?;
                    continue;
                }
                if mode == "flood" {
                    for _ in 0 .. 1_000_000 {
                        notify(
                            &mut output,
                            "window/logMessage",
                            "{\"type\":3,\"message\":\"flood\"}",
                        )?;
                    }
                    respond(&mut output, id, "[]")?;
                    continue;
                }
                if mode == "external-change" {
                    let path = uri
                        .strip_prefix("file://")
                        .ok_or_else(|| "external-change URI was not a file URI".to_owned())?;
                    std::fs::write(path, "CONCURRENT\n")
                        .map_err(|error| format!("failed to change source externally: {error}"))?;
                }
                if mode == "unsolicited" {
                    let edit = format!(
                        "{{\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}}",
                        escape_json(&uri)
                    );
                    request(&mut output, 901, "workspace/applyEdit", &edit)?;
                    loop {
                        let nested = read_message(&mut input)?.ok_or_else(|| {
                            "client disconnected during unsolicited applyEdit".to_owned()
                        })?;
                        if extract_string(&nested, "method").as_deref()
                            == Some("textDocument/didChange")
                        {
                            text = extract_string(&nested, "text")
                                .ok_or_else(|| "didChange had no text".to_owned())?;
                            version = extract_i64(&nested, "version")
                                .ok_or_else(|| "didChange had no version".to_owned())?;
                            publish_diagnostics(&mut output, &mode, &uri, &text, version)?;
                        }
                        if extract_id_optional(&nested) == Some(901)
                            && extract_string(&nested, "method").is_none()
                        {
                            break;
                        }
                    }
                    respond(&mut output, id, "[]")?;
                }
                else if mode == "retry" && !content_modified_sent {
                    content_modified_sent = true;
                    respond_error(&mut output, id, -32801, "content modified")?;
                }
                else {
                    let result = code_actions(&mode, &uris, &uri, &text, &code, same_data_requests);
                    same_data_requests = same_data_requests.saturating_add(1);
                    respond(&mut output, id, &result)?;
                }
            },
            | Some("codeAction/resolve") => {
                let result = format!(
                    "{{\"title\":\"Resolve me\",\"kind\":\"refactor.rewrite\",\"isPreferred\":true,\
                     \"data\":{{\"id\":1}},\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\
                     \"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                     \"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}}",
                    escape_json(&uri)
                );
                respond(&mut output, extract_id(&message)?, &result)?;
            },
            | Some("workspace/executeCommand") => {
                let id = extract_id(&message)?;
                let edit = if mode == "command-stale" {
                    format!(
                        "{{\"edit\":{{\"documentChanges\":[{{\"textDocument\":{{\"uri\":\"{}\",\"version\":99}},\"edits\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":11}}}},\"newText\":\"GOOD\"}}]}}]}}}}",
                        escape_json(&uri)
                    )
                }
                else {
                    format!(
                        "{{\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":11}}}},\"newText\":\"GOOD\"}}]}}}}}}",
                        escape_json(&uri)
                    )
                };
                request(&mut output, 900, "workspace/applyEdit", &edit)?;
                if mode == "command-early-response" {
                    respond(&mut output, id, "null")?;
                }
                loop {
                    let nested = read_message(&mut input)?
                        .ok_or_else(|| "client disconnected during applyEdit".to_owned())?;
                    if extract_string(&nested, "method").as_deref()
                        == Some("textDocument/didChange")
                    {
                        text = extract_string(&nested, "text")
                            .ok_or_else(|| "didChange had no text".to_owned())?;
                        version = extract_i64(&nested, "version")
                            .ok_or_else(|| "didChange had no version".to_owned())?;
                        publish_diagnostics(&mut output, &mode, &uri, &text, version)?;
                    }
                    if extract_id_optional(&nested) == Some(900)
                        && extract_string(&nested, "method").is_none()
                    {
                        break;
                    }
                }
                if mode == "command-double" {
                    let second_edit = format!(
                        "{{\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":4}}}},\"newText\":\"OTHER\"}}]}}}}}}",
                        escape_json(&uri)
                    );
                    request(&mut output, 901, "workspace/applyEdit", &second_edit)?;
                    loop {
                        let nested = read_message(&mut input)?.ok_or_else(|| {
                            "client disconnected during second applyEdit".to_owned()
                        })?;
                        if extract_string(&nested, "method").as_deref()
                            == Some("textDocument/didChange")
                        {
                            text = extract_string(&nested, "text")
                                .ok_or_else(|| "didChange had no text".to_owned())?;
                            version = extract_i64(&nested, "version")
                                .ok_or_else(|| "didChange had no version".to_owned())?;
                            publish_diagnostics(&mut output, &mode, &uri, &text, version)?;
                        }
                        if extract_id_optional(&nested) == Some(901)
                            && extract_string(&nested, "method").is_none()
                        {
                            break;
                        }
                    }
                }
                if mode == "command-fail" {
                    respond_error(&mut output, id, -32603, "command failed after edit")?;
                    continue;
                }
                if mode != "command-early-response" {
                    respond(&mut output, id, "null")?;
                }
            },
            | Some("shutdown") => {
                respond(&mut output, extract_id(&message)?, "null")?;
                if mode == "shutdown-fail" {
                    return Err("intentional shutdown failure".to_owned());
                }
            },
            | Some("exit") => break,
            | Some(_) | None => {},
        }
    }
    Ok(())
}

fn code_actions(
    mode: &str,
    uris: &[String],
    uri: &str,
    text: &str,
    code: &str,
    request_index: usize,
) -> String
{
    match (mode, code) {
        | ("edit" | "boolean-code-actions" | "shutdown-fail", "F001") => format!(
            "[{{\"title\":\"Replace BAD\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("incremental", "F001")
        | ("stale-publication", "F009")
        | ("external-change", "F010") => format!(
            "[{{\"title\":\"Replace BAD safely\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("retry", "F004") => format!(
            "[{{\"title\":\"Retry BAD replacement\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("edit", "F002") => "[{\"title\":\"Ask the user\",\"kind\":\"quickfix\",\"command\":{\"title\":\"Ask\",\"command\":\"fake.interactive\"}}]".to_owned(),
        | ("command-direct", "F003") => "[{\"title\":\"Apply direct command fix\",\"command\":\"fake.apply\",\"arguments\":[]}]".to_owned(),
        | ("command-mixed", "F003") => format!(
            "[{{\"title\":\"Reject mixed command fix\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":11}}}},\"newText\":\"GOOD\"}}]}}}},\"command\":{{\"title\":\"Apply\",\"command\":\"fake.apply\"}}}}]",
            escape_json(uri)
        ),
        | (
            "command"
            | "command-double"
            | "command-early-response"
            | "command-fail"
            | "command-unadvertised",
            "F003",
        ) => "[{\"title\":\"Apply command fix\",\"kind\":\"quickfix\",\"isPreferred\":true,\"command\":{\"title\":\"Apply\",\"command\":\"fake.apply\"}}]".to_owned(),
        | ("command-stale", "F008") => "[{\"title\":\"Apply stale command fix\",\"kind\":\"quickfix\",\"isPreferred\":true,\"command\":{\"title\":\"Apply stale\",\"command\":\"fake.apply\"}}]".to_owned(),
        | ("ambiguous", "F005") => format!(
            "[{{\"title\":\"Same action\",\"kind\":\"quickfix\",\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}}}}},{{\"title\":\"Same action\",\"kind\":\"quickfix\",\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"OTHER\"}}]}}}}}}]",
            escape_json(uri),
            escape_json(uri)
        ),
        | ("stale", "F006") => format!(
            "[{{\"title\":\"Stale replacement\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"documentChanges\":[{{\"textDocument\":{{\"uri\":\"{}\",\"version\":99}},\"edits\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}]}}]}}}}]",
            escape_json(uri)
        ),
        | ("loop", "LOOP") => {
            let replacement = if text.starts_with('A') { "B" } else { "A" };
            format!(
                "[{{\"title\":\"Toggle forever\",\"kind\":\"quickfix\",\"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":1}}}},\"newText\":\"{}\"}}]}}}}}}]",
                escape_json(uri), replacement
            )
        },
        | ("resolve-escalate", "F011") => {
            "[{\"title\":\"Resolve me\",\"kind\":\"quickfix\",\"isPreferred\":true,\"data\":{\"id\":1}}]"
                .to_owned()
        },
        | ("multi", "F013") => {
            let other = uris
                .iter()
                .find(|candidate| candidate.as_str() != uri)
                .map_or(uri, String::as_str);
            format!(
                "[{{\"title\":\"Replace two files\",\"kind\":\"quickfix\",\"isPreferred\":true,\
                 \"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\
                 \"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\"newText\":\"GOOD\"}}],\
                 \"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                 \"character\":7}}}},\"newText\":\"AUX_GOOD\"}}]}}}}}}]",
                escape_json(uri),
                escape_json(other)
            )
        },
        | ("unversioned-residual", "F014") => format!(
            "[{{\"title\":\"Replace before unversioned residual\",\"kind\":\"quickfix\",\
             \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\
             \"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\
             \"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("delayed-unversioned", "F018") => format!(
            "[{{\"title\":\"Replace before delayed unversioned diagnostic\",\"kind\":\"quickfix\",\
             \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\
             \"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\
             \"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("delayed-unversioned", "F019") => format!(
            "[{{\"title\":\"Unsafe delayed replacement\",\"kind\":\"quickfix\",\"isPreferred\":true,\
             \"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\"line\":0,\
             \"character\":0}},\"end\":{{\"line\":0,\"character\":4}}}},\
             \"newText\":\"EVIL\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("malformed-item", "F020") => format!(
            "[{{\"title\":\"Unsafe malformed diagnostic edit\",\"kind\":\"quickfix\",\
             \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\
             \"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\
             \"newText\":\"EVIL\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("stale-only", "F021") => format!(
            "[{{\"title\":\"Replace before stale-only publication\",\"kind\":\"quickfix\",\
             \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\"start\":{{\
             \"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":3}}}},\
             \"newText\":\"GOOD\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("oversized-expansion", "F025") => {
            let replacement = "x".repeat(128 * 1024);
            format!(
                "[{{\"title\":\"Oversize synchronized document\",\"kind\":\"quickfix\",\
                 \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\
                 \"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                 \"character\":3}}}},\"newText\":\"{replacement}\"}}]}}}}}}]",
                escape_json(uri)
            )
        },
        | ("same-visible-data", "F026") if request_index == 0 => format!(
            "[{{\"title\":\"Wrong opaque diagnostic\",\"kind\":\"quickfix\",\"isPreferred\":true,\
             \"diagnostics\":[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\
             \"end\":{{\"line\":0,\"character\":3}}}},\"severity\":2,\"code\":\"F026\",\
             \"source\":\"fake-lsp\",\"message\":\"same visible diagnostic\",\
             \"data\":{{\"id\":2}}}}],\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\
             \"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
             \"character\":3}}}},\"newText\":\"EVIL\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | ("unversioned-clear", "F029") => format!(
            "[{{\"title\":\"Unsafe cleared diagnostic\",\"kind\":\"quickfix\",\
             \"isPreferred\":true,\"edit\":{{\"changes\":{{\"{}\":[{{\"range\":{{\
             \"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
             \"character\":3}}}},\"newText\":\"EVIL\"}}]}}}}}}]",
            escape_json(uri)
        ),
        | _ => "[]".to_owned(),
    }
}

fn publish_diagnostics(
    output: &mut impl Write,
    mode: &str,
    uri: &str,
    text: &str,
    version: i64,
) -> Result<(), String>
{
    if mode == "malformed-diagnostics" {
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":{{}}}}",
                escape_json(uri)
            ),
        );
    }
    if mode == "malformed-item" {
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":[{{\"range\":{{\
                 \"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                 \"character\":3}}}},\"code\":\"F020\",\"source\":\"fake-lsp\"}}]}}",
                escape_json(uri)
            ),
        );
    }
    if mode == "unversioned-clear" {
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":{}}}",
                escape_json(uri),
                diagnostic("F029", "diagnostic cleared by unversioned publication", 3)
            ),
        )?;
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!("{{\"uri\":\"{}\",\"diagnostics\":[]}}", escape_json(uri)),
        );
    }
    if mode == "same-visible-data" {
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":[\
                 {{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                 \"character\":3}}}},\"severity\":2,\"code\":\"F026\",\"source\":\"fake-lsp\",\
                 \"message\":\"same visible diagnostic\",\"data\":{{\"id\":1}}}},\
                 {{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\
                 \"character\":3}}}},\"severity\":2,\"code\":\"F026\",\"source\":\"fake-lsp\",\
                 \"message\":\"same visible diagnostic\",\"data\":{{\"id\":2}}}}]}}",
                escape_json(uri)
            ),
        );
    }
    let diagnostics = match mode {
        | "edit" if text.starts_with("BAD") => diagnostic("F001", "replace BAD", 3),
        | "edit" if text.starts_with("GOOD") => diagnostic("F002", "interactive fix remains", 4),
        | "command"
        | "command-direct"
        | "command-double"
        | "command-early-response"
        | "command-fail"
        | "command-mixed"
        | "command-unadvertised"
            if text.starts_with("COMMAND_BAD") =>
        {
            diagnostic("F003", "apply command fix", 11)
        },
        | "command-stale" if text.starts_with("COMMAND_BAD") => {
            diagnostic("F008", "apply stale command fix", 11)
        },
        | "boolean-code-actions" if text.starts_with("BAD") => diagnostic("F001", "replace BAD", 3),
        | "retry" if text.starts_with("BAD") => diagnostic("F004", "retry replacement", 3),
        | "ambiguous" if text.starts_with("BAD") => diagnostic("F005", "ambiguous replacement", 3),
        | "stale" if text.starts_with("BAD") => diagnostic("F006", "stale replacement", 3),
        | "unsolicited" if text.starts_with("BAD") => {
            diagnostic("F007", "unsolicited server edit", 3)
        },
        | "incremental" if text.starts_with("BAD") => diagnostic("F001", "replace BAD", 3),
        | "stale-publication" if text.starts_with("BAD") => {
            diagnostic("F009", "replace BAD with versioned diagnostics", 3)
        },
        | "external-change" if text.starts_with("BAD") => {
            diagnostic("F010", "replace externally changed source", 3)
        },
        | "loop" => diagnostic("LOOP", "non-converging fix", 1),
        | "resolve-escalate" if text.starts_with("BAD") => {
            diagnostic("F011", "resolved action may not escalate policy", 3)
        },
        | "blocked-stdin" if text.starts_with("BAD") => {
            diagnostic("F012", &"x".repeat(256 * 1024), 3)
        },
        | "multi" if text.starts_with("BAD") => diagnostic("F013", "replace two files", 3),
        | "unversioned-residual" if text.starts_with("BAD") => {
            diagnostic("F014", "replace before unversioned residual", 3)
        },
        | "unversioned-residual" if text.starts_with("GOOD") => {
            diagnostic("F015", "unversioned residual remains", 4)
        },
        | "malformed-response"
        | "malformed-request-id"
        | "registration-request"
        | "wrong-jsonrpc-version"
        | "missing-jsonrpc-version"
        | "array-jsonrpc-root"
        | "flood"
            if text.starts_with("BAD") =>
        {
            diagnostic("F017", "exercise bounded malformed response", 3)
        },
        | "delayed-unversioned" if text.starts_with("BAD") => {
            diagnostic("F018", "replace before delayed unversioned diagnostic", 3)
        },
        | "stale-only" if text.starts_with("BAD") => {
            diagnostic("F021", "replace before stale-only publication", 3)
        },
        | "oversized-expansion" if text.starts_with("BAD") => {
            diagnostic("F025", "expand beyond synchronized message bound", 3)
        },
        | "shutdown-fail" if text.starts_with("BAD") => diagnostic("F001", "replace BAD", 3),
        | _ => "[]".to_owned(),
    };
    if mode == "stale-publication" && version > 0 {
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":0,\"diagnostics\":{}}}",
                escape_json(uri),
                diagnostic("F009", "stale diagnostic must be ignored", 3)
            ),
        )?;
    }
    if mode == "stale-only" && version > 0 {
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":0,\"diagnostics\":{}}}",
                escape_json(uri),
                diagnostic("F022", "stale-only residual must be ignored", 4)
            ),
        );
    }
    if mode == "delayed-unversioned" && version > 0 {
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":[]}}",
                escape_json(uri)
            ),
        )?;
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"diagnostics\":{}}}",
                escape_json(uri),
                diagnostic("F019", "delayed unversioned diagnostic", 4)
            ),
        );
    }
    if mode == "unversioned-residual" && version > 0 {
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"diagnostics\":{diagnostics}}}",
                escape_json(uri)
            ),
        );
    }
    if mode == "unopened-regression" {
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":[]}}",
                escape_json(uri)
            ),
        )?;
        let unopened_uri = format!("{}.generated", escape_json(uri));
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{unopened_uri}\",\"version\":5,\"diagnostics\":{}}}",
                diagnostic("F023", "newer unopened residual", 1)
            ),
        )?;
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{unopened_uri}\",\"version\":4,\"diagnostics\":{}}}",
                diagnostic("F024", "older unopened residual", 1)
            ),
        );
    }
    if mode == "unopened-residual" {
        notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":[]}}",
                escape_json(uri)
            ),
        )?;
        return notify(
            output,
            "textDocument/publishDiagnostics",
            &format!(
                "{{\"uri\":\"{}.generated\",\"diagnostics\":{}}}",
                escape_json(uri),
                diagnostic("F016", "unopened residual remains visible", 1)
            ),
        );
    }
    notify(
        output,
        "textDocument/publishDiagnostics",
        &format!(
            "{{\"uri\":\"{}\",\"version\":{version},\"diagnostics\":{diagnostics}}}",
            escape_json(uri)
        ),
    )
}

fn diagnostic(
    code: &str,
    message: &str,
    end: u32,
) -> String
{
    format!(
        "[{{\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":{end}}}}},\"severity\":2,\"code\":\"{code}\",\"source\":\"fake-lsp\",\"message\":\"{message}\"}}]"
    )
}

fn request(
    output: &mut impl Write,
    id: u64,
    method: &str,
    params: &str,
) -> Result<(), String>
{
    write_message(
        output,
        &format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}"),
    )
}

fn respond(
    output: &mut impl Write,
    id: u64,
    result: &str,
) -> Result<(), String>
{
    write_message(
        output,
        &format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}"),
    )
}

fn respond_error(
    output: &mut impl Write,
    id: u64,
    code: i64,
    message: &str,
) -> Result<(), String>
{
    write_message(
        output,
        &format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":{code},\"message\":\"{}\"}}}}",
            escape_json(message)
        ),
    )
}

fn notify(
    output: &mut impl Write,
    method: &str,
    params: &str,
) -> Result<(), String>
{
    write_message(
        output,
        &format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params}}}"),
    )
}

fn write_message(
    output: &mut impl Write,
    payload: &str,
) -> Result<(), String>
{
    write!(output, "Content-Length: {}\r\n\r\n{payload}", payload.len())
        .and_then(|()| output.flush())
        .map_err(|error| format!("failed to write message: {error}"))
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<String>, String>
{
    let mut content_length = None;
    loop {
        let mut header = String::new();
        let bytes = reader
            .read_line(&mut header)
            .map_err(|error| format!("failed to read header: {error}"))?;
        if bytes == 0 {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header.trim().strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| format!("invalid content length: {error}"))?,
            );
        }
    }
    let length = content_length.ok_or_else(|| "missing content length".to_owned())?;
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("failed to read payload: {error}"))?;
    String::from_utf8(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn extract_id(message: &str) -> Result<u64, String>
{
    extract_id_optional(message).ok_or_else(|| "message had no numeric id".to_owned())
}

fn extract_id_optional(message: &str) -> Option<u64>
{
    let tail = message.split_once("\"id\":")?.1;
    let digits = tail
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn extract_i64(
    message: &str,
    field: &str,
) -> Option<i64>
{
    let marker = format!("\"{field}\":");
    let tail = message.split_once(&marker)?.1;
    let value = tail
        .chars()
        .take_while(|character| *character == '-' || character.is_ascii_digit())
        .collect::<String>();
    value.parse().ok()
}

fn extract_string(
    message: &str,
    field: &str,
) -> Option<String>
{
    let marker = format!("\"{field}\":\"");
    let tail = message.split_once(&marker)?.1;
    let mut decoded = String::new();
    let mut characters = tail.chars();
    while let Some(character) = characters.next() {
        match character {
            | '"' => return Some(decoded),
            | '\\' => match characters.next()? {
                | '"' => decoded.push('"'),
                | '\\' => decoded.push('\\'),
                | '/' => decoded.push('/'),
                | 'b' => decoded.push('\u{0008}'),
                | 'f' => decoded.push('\u{000C}'),
                | 'n' => decoded.push('\n'),
                | 'r' => decoded.push('\r'),
                | 't' => decoded.push('\t'),
                | _ => return None,
            },
            | other => decoded.push(other),
        }
    }
    None
}

fn escape_json(value: &str) -> String
{
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            | '"' => escaped.push_str("\\\""),
            | '\\' => escaped.push_str("\\\\"),
            | '\n' => escaped.push_str("\\n"),
            | '\r' => escaped.push_str("\\r"),
            | '\t' => escaped.push_str("\\t"),
            | other => escaped.push(other),
        }
    }
    escaped
}
