//! P4.3.2E2: capabilities are enforced at the operation boundaries, for
//! top-level graphs, nested custom nodes and packages, workers, and after
//! revocation. Every test switches off the test-only "allow all" default,
//! so code without a context has no rights (as in production), and each
//! forbidden operation is shown not to happen: no connection reaches a
//! local listener, no file is read or created, no program runs, no key is
//! returned, no device or capture is opened.

use super::*;
use crate::capability::{CapSet, Capability, Context};
use crate::document::{CustomNodeFile, read_custom_node_interface_static};
use crate::library::package::tests::TempDir;
use crate::library::trust::TrustStore;
use crate::library::trust::authorize::{TEST_DEFAULT_ALLOW, TrustPolicy};
use crate::library::trust::content::{ContentDecision, ContentKind, custom_node_digest};

const FS: Capability = Capability::Filesystem;
const SECRET: &str = "classified";

struct Enforce;

impl Enforce {
    fn new() -> Self {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(false));
        Self
    }
}

impl Drop for Enforce {
    fn drop(&mut self) {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(true));
    }
}

/// Everything but `capability`.
fn all_but(capability: Capability) -> Context {
    Context::new(
        CapSet::ALL.minus(CapSet::of(&[capability])),
        "test document",
    )
}

struct Fx {
    tmp: TempDir,
    store: TrustStore,
    secret: std::path::PathBuf,
    _enforce: Enforce,
}

fn fx(tag: &str) -> Fx {
    let enforce = Enforce::new();
    let tmp = TempDir::new(tag);
    let secret = tmp.0.join("secret.txt");
    std::fs::write(&secret, SECRET).unwrap();
    let store = TrustStore::at(tmp.0.join("home").join("trust").join("packages.json"));
    Fx {
        tmp,
        store,
        secret,
        _enforce: enforce,
    }
}

impl Fx {
    fn graph(&self) -> Graph {
        let mut graph = Graph::new();
        graph.set_trust_policy(TrustPolicy::Store(self.store.clone()));
        graph
    }

    fn write(&self, name: &str, graph: Graph) -> std::path::PathBuf {
        let dir = self.tmp.0.join("nodes");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.pwnode"));
        let (inputs, outputs) = crate::document::custom_node_interface(&graph);
        CustomNodeFile {
            version: 1,
            name: name.into(),
            graph,
            inputs,
            outputs,
        }
        .save_with_digest(&path)
        .unwrap();
        path
    }

    /// A custom node whose graph reads the secret file (filesystem) and
    /// outputs its text.
    fn reader(&self, name: &str) -> std::path::PathBuf {
        let mut graph = Graph::new();
        let browser = graph.add_node(folder_browser(&self.secret), [0.0, 0.0]);
        let out = graph.add_node(text_out(), [200.0, 0.0]);
        graph.add_connection(browser, 2, out, 0);
        self.write(name, graph)
    }

    /// A custom node that only contains `child` (and forwards its output).
    fn wrapper(&self, name: &str, child: &std::path::Path) -> std::path::PathBuf {
        let mut graph = Graph::new();
        let inner = graph.add_node(loose(child), [0.0, 0.0]);
        let out = graph.add_node(text_out(), [200.0, 0.0]);
        graph.add_connection(inner, 0, out, 0);
        self.write(name, graph)
    }

    fn record(&self, file: &std::path::Path, decision: ContentDecision) {
        let digest = custom_node_digest(&std::fs::read_to_string(file).unwrap()).unwrap();
        self.store
            .content_ledger()
            .record(ContentKind::CustomNode, &digest, decision, true)
            .unwrap();
    }
}

fn folder_browser(file: &std::path::Path) -> NodeType {
    NodeType::FolderBrowser {
        path: String::new(),
        selected_file: file.to_string_lossy().into_owned(),
        search: String::new(),
        cached_content: String::new(),
        cached_content_path: String::new(),
    }
}

fn text_out() -> NodeType {
    NodeType::CustomOutput {
        name: "text".into(),
        kind: PortKind::Text,
    }
}

fn loose(path: &std::path::Path) -> NodeType {
    let interface = read_custom_node_interface_static(path).unwrap();
    NodeType::CustomNodeInstance {
        file: path.to_string_lossy().into_owned(),
        inputs: interface.inputs,
        outputs: interface.outputs,
        package: None,
    }
}

/// Evaluate under `context` (the document's rights) and return the text
/// an instance outputs (`None`: not evaluated / blocked).
fn read_through(graph: &mut Graph, id: NodeId, context: Option<&Context>) -> Option<String> {
    let _entered = context.map(Context::enter);
    match graph.evaluate(0.0).get(&(id, 0)) {
        Some(PortValue::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn runtime_context(graph: &Graph, id: NodeId) -> Context {
    graph.custom_runtimes[&id]
        .capabilities
        .context()
        .cloned()
        .expect("a running custom node has its own context")
}

// ── Each capability, at its boundary ─────────────────────────────────────

#[test]
fn network_requests_need_the_network_capability() {
    let _enforce = Enforce::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let request = |node_id| crate::http::HttpAction::SendRequest {
        node_id,
        url: url.clone(),
        method: "GET".into(),
        headers: Vec::new(),
        body: String::new(),
    };
    let accepted_within = |ms: u64| {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            if listener.accept().is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    };

    let mut http = crate::http::HttpManager::new();
    let denied = all_but(Capability::Network);
    {
        let _entered = denied.enter();
        http.process(vec![request(1)]);
    }
    assert!(!http.is_pending(1), "nothing was started");
    assert!(!accepted_within(400), "no connection was made");
    assert!(denied.denied_count(Capability::Network) >= 1);

    // Control: with the capability the same request connects.
    let allowed = Context::new(CapSet::ALL, "control");
    {
        let _entered = allowed.enter();
        http.process(vec![request(2)]);
    }
    assert!(
        accepted_within(10_000),
        "the fixture does connect when allowed"
    );
}

#[test]
fn file_access_needs_the_filesystem_capability() {
    let f = fx("cap_fs");
    // Reading, in a top-level graph.
    let mut graph = f.graph();
    let browser = graph.add_node(folder_browser(&f.secret), [0.0, 0.0]);
    let read = |graph: &mut Graph, context: &Context| {
        let _entered = context.enter();
        match graph.evaluate(0.0).get(&(browser, 2)) {
            Some(PortValue::Text(text)) => text.clone(),
            _ => String::new(),
        }
    };
    let denied = all_but(FS);
    assert_eq!(read(&mut graph, &denied), "", "not read");
    assert!(denied.denied_count(FS) >= 1);
    assert_eq!(
        read(&mut graph, &Context::new(CapSet::ALL, "control")),
        SECRET
    );

    // Writing: a recording is never created.
    let target = f.tmp.0.join("recording.mp4");
    let _entered = denied.enter();
    let started = crate::video_io::file_recorder::FileRecorder::new(
        &target,
        crate::video_io::file_recorder::Codec::H264Sw,
        64,
        48,
    );
    assert!(started.err().unwrap().starts_with("Not permitted"));
    assert!(!target.exists(), "no file was created");
}

#[test]
fn programs_run_only_with_the_process_capability() {
    let f = fx("cap_process");
    let run = |context: &Context, marker: &std::path::Path| {
        let _entered = context.enter();
        let (tx, rx) = std::sync::mpsc::channel();
        crate::nodes::terminal_node::run_command(
            // Relative to the working directory (no quoting differences
            // between shells).
            format!(
                "echo ran > {}",
                marker.file_name().unwrap().to_string_lossy()
            ),
            f.tmp.0.to_string_lossy().into_owned(),
            tx,
        );
        rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap()
    };
    let refused = f.tmp.0.join("refused.txt");
    let denied = all_but(Capability::Process);
    let (_, error) = run(&denied, &refused);
    assert!(error.starts_with("Not permitted"), "{error}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!refused.exists(), "the program did not run");

    // Control: the same command runs when allowed.
    let ran = f.tmp.0.join("ran.txt");
    run(&Context::new(CapSet::ALL, "control"), &ran);
    assert!(ran.exists(), "the fixture runs when allowed");
}

#[test]
fn stored_ai_keys_need_the_ai_credentials_capability() {
    let _enforce = Enforce::new();
    let keys = crate::settings::ProviderKeys {
        anthropic: "sk-test-key".into(),
        ..Default::default()
    };
    let denied = all_but(Capability::AiCredentials);
    {
        let _entered = denied.enter();
        assert_eq!(
            crate::nodes::ai_shared::resolve_key("anthropic", &keys),
            (String::new(), false),
            "no key (neither stored nor from the environment)"
        );
        assert!(
            crate::nodes::ai_shared::ai_configs_dir().is_none(),
            "no config store"
        );
    }
    assert!(denied.denied_count(Capability::AiCredentials) >= 2);
    let _entered = Context::new(CapSet::ALL, "control").enter();
    assert_eq!(
        crate::nodes::ai_shared::resolve_key("anthropic", &keys).0,
        "sk-test-key"
    );
}

#[test]
fn camera_and_microphone_need_the_capture_capability() {
    let _enforce = Enforce::new();
    let denied = all_but(Capability::MediaCapture);
    let _entered = denied.enter();
    let camera = crate::nodes::video_player::VideoDecoder::open_camera(0, 64, 48);
    assert!(camera.err().unwrap().starts_with("Not permitted"));
    let screen = crate::nodes::video_player::VideoDecoder::open_screen(0, 64, 48, 0, 0);
    assert!(screen.err().unwrap().starts_with("Not permitted"));
    let microphone = crate::nodes::video_player::AudioPipeDecoder::open_capture_input(
        "dshow", "audio=x", 48_000.0,
    );
    assert!(microphone.err().unwrap().starts_with("Not permitted"));
    let mut audio = crate::audio::AudioManager::placeholder();
    assert!(
        audio
            .start_input(7, None)
            .unwrap_err()
            .starts_with("Not permitted")
    );
    assert_eq!(
        denied.allowed_count(Capability::MediaCapture),
        0,
        "no capture was opened"
    );
    assert!(denied.denied_count(Capability::MediaCapture) >= 4);
}

#[test]
fn system_information_needs_its_capability() {
    let _enforce = Enforce::new();
    let mut monitor = crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .create("monitor", &serde_json::Value::Null)
        .unwrap();
    let denied = all_but(Capability::SystemInfo);
    let _entered = denied.enter();
    monitor.evaluate(&[]);
    monitor.evaluate(&[]);
    assert_eq!(
        denied.allowed_count(Capability::SystemInfo),
        0,
        "no sampler was started"
    );
    assert!(denied.denied_count(Capability::SystemInfo) >= 1);
}

#[test]
fn devices_need_the_devices_capability() {
    let _enforce = Enforce::new();
    let mut serial = crate::serial::SerialManager::new();
    let denied = all_but(Capability::Devices);
    let _entered = denied.enter();
    serial.process(vec![crate::serial::SerialAction::Connect {
        node_id: 5,
        port_name: "PATCHWORK-NO-SUCH-PORT".into(),
        baud_rate: 9600,
    }]);
    assert!(!serial.is_connected(5));
    assert_eq!(
        denied.allowed_count(Capability::Devices),
        0,
        "no port opened"
    );
    assert!(denied.denied_count(Capability::Devices) >= 1);
    assert!(crate::hid::global().find_device("knob", 1).is_none());
}

#[test]
fn scripts_and_native_code_need_their_capabilities() {
    let _enforce = Enforce::new();
    let mut graph = Graph::new();
    let script = graph.add_node(
        NodeType::Script {
            name: "s".into(),
            input_names: Vec::new(),
            output_names: vec!["y".into()],
            code: "let y = 1.0;".into(),
            last_values: Vec::new(),
            error: String::new(),
            continuous: true,
            trigger: false,
        },
        [0.0, 0.0],
    );
    let error = |graph: &mut Graph, context: &Context| {
        let _entered = context.enter();
        graph.evaluate(0.0);
        match &graph.nodes[&script].node_type {
            NodeType::Script { error, .. } => error.clone(),
            _ => unreachable!(),
        }
    };
    let denied = all_but(Capability::Scripting);
    assert!(error(&mut graph, &denied).starts_with("Not permitted"));
    assert!(!error(&mut graph, &Context::new(CapSet::ALL, "control")).starts_with("Not permitted"));

    let native = all_but(Capability::NativeCode);
    let _entered = native.enter();
    let loaded = crate::audio::clap_host::ClapInstance::load("no-such-plugin.clap", 48_000.0, 512);
    assert!(loaded.err().unwrap().starts_with("Not permitted"));
}

// ── Nested graphs ────────────────────────────────────────────────────────

#[test]
fn nested_custom_nodes_run_with_their_own_approval_within_their_parent() {
    let f = fx("cap_nested");
    let reader = f.reader("Reader");
    let all = Context::new(CapSet::ALL, "authored project");
    let check = |decision: ContentDecision, document: Option<&Context>, what: &str| {
        f.record(&reader, decision);
        let mut graph = f.graph();
        let id = graph.add_node(loose(&reader), [0.0, 0.0]);
        let text = read_through(&mut graph, id, document);
        (text, graph, id, what.to_string())
    };

    // Authored here, in an authored document: everything.
    let (text, ..) = check(ContentDecision::Authored, Some(&all), "authored");
    assert_eq!(text.as_deref(), Some(SECRET));

    // Allowed with no capability: it runs, but cannot read, even inside a
    // document that may do everything (no elevation through the parent).
    let (text, graph, id, _) = check(ContentDecision::Allowed(CapSet::EMPTY), Some(&all), "");
    assert_eq!(text.as_deref(), Some(""), "ran, but read nothing");
    assert!(runtime_context(&graph, id).denied_count(FS) >= 1);

    // Allowed with exactly the filesystem capability.
    let (text, ..) = check(ContentDecision::Allowed(CapSet::of(&[FS])), Some(&all), "");
    assert_eq!(text.as_deref(), Some(SECRET));

    // Authored, inside a document without the capability: never more
    // than its parent.
    let (text, ..) = check(ContentDecision::Authored, Some(&all_but(FS)), "");
    assert_eq!(text.as_deref(), Some(""));

    // No document context at all: fail closed.
    let (text, ..) = check(ContentDecision::Authored, None, "");
    assert_eq!(text.as_deref(), Some(""));
}

#[test]
fn a_trusted_child_gets_no_more_than_every_graph_around_it() {
    let f = fx("cap_chain");
    let reader = f.reader("Reader");
    let wrapper = f.wrapper("Wrapper", &reader);
    f.record(&reader, ContentDecision::Authored);
    let all = Context::new(CapSet::ALL, "authored project");

    // The wrapper was allowed without capabilities: the authored reader
    // inside it cannot read.
    f.record(&wrapper, ContentDecision::Allowed(CapSet::EMPTY));
    let mut graph = f.graph();
    let id = graph.add_node(loose(&wrapper), [0.0, 0.0]);
    assert_eq!(
        read_through(&mut graph, id, Some(&all)).as_deref(),
        Some("")
    );
    let inner = graph.custom_runtimes[&id]
        .custom_runtimes
        .keys()
        .next()
        .copied()
        .unwrap();
    let inner_context = runtime_context(&graph.custom_runtimes[&id], inner);
    assert_eq!(inner_context.granted(), CapSet::EMPTY);

    // With the filesystem approved for the wrapper, it can.
    f.record(&wrapper, ContentDecision::Allowed(CapSet::of(&[FS])));
    let mut graph = f.graph();
    let id = graph.add_node(loose(&wrapper), [0.0, 0.0]);
    assert_eq!(
        read_through(&mut graph, id, Some(&all)).as_deref(),
        Some(SECRET)
    );
}

#[test]
fn packages_run_with_their_grant_within_their_parent() {
    let f = fx("cap_package");
    let library = f.tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let source = f.reader("PkgReader");
    let package =
        crate::library::bundle::tests::publish(&library, &source, "PkgReader", None, "1.0.0")
            .unwrap();
    let dir = package.entry_path.parent().unwrap().to_path_buf();
    let identity = crate::library::trust::identify(&dir, true)
        .unwrap()
        .identity;
    f.store.grant(&dir, &identity).unwrap(); // approves what it uses: files
    let instance = || {
        let interface = read_custom_node_interface_static(&package.entry_path).unwrap();
        NodeType::CustomNodeInstance {
            file: package.entry_path.to_string_lossy().into_owned(),
            inputs: interface.inputs,
            outputs: interface.outputs,
            package: Some(crate::library::resolve::InstancePackageRef {
                id: package.manifest.id.clone(),
                version: package.version.to_string(),
                entry: package.manifest.entry.clone(),
            }),
        }
    };
    let run = |document: &Context| {
        let mut graph = f.graph();
        graph.set_package_environment(None, Some(library.clone()));
        let id = graph.add_node(instance(), [0.0, 0.0]);
        let text = read_through(&mut graph, id, Some(document));
        let rights = runtime_context(&graph, id).granted();
        (text, rights)
    };
    let (text, rights) = run(&Context::new(CapSet::ALL, "authored project"));
    assert_eq!(text.as_deref(), Some(SECRET));
    assert_eq!(rights, CapSet::of(&[FS]), "exactly its grant");
    let (text, rights) = run(&all_but(FS));
    assert_eq!(text.as_deref(), Some(""));
    assert_eq!(rights, CapSet::EMPTY, "never more than its parent");
}

// ── Revocation ───────────────────────────────────────────────────────────

#[test]
fn revoking_a_decision_stops_the_runtime_and_its_workers() {
    let f = fx("cap_revoke");
    let reader = f.reader("Reader");
    f.record(&reader, ContentDecision::Allowed(CapSet::of(&[FS])));
    let document = Context::new(CapSet::ALL, "authored project");
    let mut graph = f.graph();
    let id = graph.add_node(loose(&reader), [0.0, 0.0]);
    assert_eq!(
        read_through(&mut graph, id, Some(&document)).as_deref(),
        Some(SECRET)
    );

    // A background worker started by the runtime (with its rights).
    let context = runtime_context(&graph, id);
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker = {
        let _entered = context.enter();
        crate::capability::spawn(move || {
            result_tx
                .send(crate::capability::check(FS).is_ok())
                .unwrap();
            go_rx.recv().unwrap();
            result_tx
                .send(crate::capability::check(FS).is_ok())
                .unwrap();
        })
    };
    assert!(result_rx.recv().unwrap());

    // The user denies the file: the runtime goes, and its rights with it.
    let digest = custom_node_digest(&std::fs::read_to_string(&reader).unwrap()).unwrap();
    f.store
        .content_ledger()
        .decide_file(ContentKind::CustomNode, &reader, &digest, false)
        .unwrap();
    assert_eq!(read_through(&mut graph, id, Some(&document)), None);
    assert!(!graph.has_custom_runtime(id));
    assert!(context.is_revoked());
    go_tx.send(()).unwrap();
    assert!(!result_rx.recv().unwrap(), "the worker can no longer act");
    worker.join().unwrap();
}

#[test]
fn narrowing_a_documents_rights_revokes_the_old_ones() {
    let _enforce = Enforce::new();
    let mut graph = Graph::new();
    let wide = graph
        .capability_scope_mut()
        .ensure(CapSet::ALL, "this project");
    let narrow = graph
        .capability_scope_mut()
        .ensure(CapSet::of(&[FS]), "this project");
    assert!(wide.is_revoked());
    assert!(!narrow.is_revoked());
    // Replacing the graph (undo, opening another project) revokes too.
    drop(graph);
    assert!(narrow.is_revoked());
}

// ── External resources ───────────────────────────────────────────────────

#[test]
fn a_changed_external_file_never_widens_what_the_graph_may_do() {
    let f = fx("cap_external");
    let script = f.tmp.0.join("script.rhai");
    std::fs::write(&script, "let y = 1.0;").unwrap();
    // A file-fed script: the file is data; running it is still scripting.
    let mut graph = f.graph();
    let browser = graph.add_node(folder_browser(&script), [0.0, 0.0]);
    let node = graph.add_node(
        NodeType::Script {
            name: "s".into(),
            input_names: Vec::new(),
            output_names: vec!["y".into()],
            code: String::new(),
            last_values: Vec::new(),
            error: String::new(),
            continuous: true,
            trigger: false,
        },
        [200.0, 0.0],
    );
    graph.add_connection(browser, 2, node, 0);
    let files_only = Context::new(CapSet::of(&[FS]), "allowed project");
    let error = |graph: &mut Graph| {
        let _entered = files_only.enter();
        graph.evaluate(0.0);
        match &graph.nodes[&node].node_type {
            NodeType::Script { error, .. } => error.clone(),
            _ => unreachable!(),
        }
    };
    assert!(error(&mut graph).starts_with("Not permitted"));
    // Whatever the file now holds, the graph's rights are the same.
    std::fs::write(&script, "let y = 2.0; // changed").unwrap();
    assert!(error(&mut graph).starts_with("Not permitted"));
    assert!(
        files_only.allowed_count(FS) >= 1,
        "the file itself was read"
    );
    // With scripting approved, the same graph runs it.
    let both = Context::new(CapSet::of(&[FS, Capability::Scripting]), "approved");
    let _entered = both.enter();
    graph.evaluate(0.0);
    match &graph.nodes[&node].node_type {
        NodeType::Script { error, .. } => assert!(!error.starts_with("Not permitted")),
        _ => unreachable!(),
    }
}

// ── Local compatibility ──────────────────────────────────────────────────

#[test]
fn locally_authored_projects_keep_every_capability() {
    let f = fx("cap_local");
    let reader = f.reader("Reader");
    f.record(&reader, ContentDecision::Authored);
    // As the editor runs a new (authored) project: its rights come from
    // its content state.
    let mut graph = f.graph();
    assert_eq!(graph.content.capabilities(), CapSet::ALL);
    let granted = graph.content.capabilities();
    let rights = graph.capability_scope_mut().ensure(granted, "this project");
    let browser = graph.add_node(folder_browser(&f.secret), [0.0, 0.0]);
    let nested = graph.add_node(loose(&reader), [0.0, 100.0]);
    let _entered = rights.enter();
    let values = graph.evaluate(0.0);
    assert_eq!(
        values.get(&(browser, 2)),
        Some(&PortValue::Text(SECRET.into()))
    );
    assert_eq!(
        values.get(&(nested, 0)),
        Some(&PortValue::Text(SECRET.into()))
    );
    assert_eq!(rights.denied_count(FS), 0);
}

// ── Initialization runs in the graph's context (P4.3.2E2 fixes) ──────────

/// The text configuration an AI Config node outputs under `context`.
fn ai_text_config(graph: &mut Graph, id: NodeId, context: &Context) -> String {
    let _entered = context.enter();
    match graph.evaluate(0.0).get(&(id, 0)) {
        Some(PortValue::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

/// Points the AI configuration store at the fixture (never the user's).
struct AiConfigs;

impl AiConfigs {
    fn at(dir: std::path::PathBuf, key: &str) -> AiConfigs {
        crate::nodes::ai_shared::TEST_AI_CONFIGS_DIR.with(|d| *d.borrow_mut() = Some(dir));
        let _entered = Context::new(CapSet::ALL, "fixture").enter();
        let mut bundle = crate::nodes::ai_shared::ConfigBundle::seeded();
        bundle.keys.anthropic = key.into();
        crate::nodes::ai_shared::save_config("default", &bundle).unwrap();
        AiConfigs
    }
}

impl Drop for AiConfigs {
    fn drop(&mut self) {
        crate::nodes::ai_shared::TEST_AI_CONFIGS_DIR.with(|d| *d.borrow_mut() = None);
    }
}

#[test]
fn ai_config_reads_its_keys_in_its_graphs_context_after_load_paste_and_undo() {
    const KEY: &str = "sk-ant-fixture-7f3a";
    let f = fx("cap_ai_config");
    let _configs = AiConfigs::at(f.tmp.0.join("ai_configs"), KEY);
    let entry = crate::nodes::catalog()
        .into_iter()
        .find(|e| e.label == "AI Config")
        .unwrap();
    let authored = Context::new(CapSet::ALL, "authored project");

    let mut graph = f.graph();
    let id = graph.add_node((entry.factory)(), [0.0, 0.0]);
    assert!(
        ai_text_config(&mut graph, id, &authored).contains(KEY),
        "added"
    );

    // Project load and session restore: rebuilt from the saved graph (the
    // keys are never saved), with no rights during construction.
    let saved = serde_json::to_value(&graph).unwrap();
    assert!(!saved.to_string().contains(KEY));
    let mut loaded: Graph = serde_json::from_value(saved).unwrap();
    assert!(
        ai_text_config(&mut loaded, id, &authored).contains(KEY),
        "after load"
    );
    // Undo/redo snapshots and clones are copies of the graph.
    let mut undone = graph.clone();
    assert!(
        ai_text_config(&mut undone, id, &authored).contains(KEY),
        "after undo"
    );
    // Paste: a copy of the node, constructed with no rights.
    let copy = graph.nodes[&id].node_type.clone();
    let pasted = graph.add_node(copy, [40.0, 0.0]);
    assert!(
        ai_text_config(&mut graph, pasted, &authored).contains(KEY),
        "after paste"
    );

    // A graph not approved for AI credentials never obtains them: neither
    // a freshly loaded node nor one that read them before.
    let imported = all_but(Capability::AiCredentials);
    let mut fresh: Graph = serde_json::from_value(serde_json::to_value(&graph).unwrap()).unwrap();
    assert!(!ai_text_config(&mut fresh, id, &imported).contains(KEY));
    assert!(!ai_text_config(&mut graph, id, &imported).contains(KEY));
    assert!(imported.denied_count(Capability::AiCredentials) > 0);
    // Revoked rights withdraw them too.
    let closing = Context::new(CapSet::ALL, "closing project");
    assert!(ai_text_config(&mut graph, pasted, &closing).contains(KEY));
    closing.revoke();
    assert!(!ai_text_config(&mut graph, pasted, &closing).contains(KEY));
    // Approved again: read again.
    assert!(ai_text_config(&mut graph, pasted, &authored).contains(KEY));
}

#[test]
fn a_trust_change_reaches_runtimes_in_tabs_that_are_not_shown() {
    let f = fx("cap_background");
    let reader = f.reader("Reader");
    let net = CapSet::of(&[Capability::Network]);
    f.record(
        &reader,
        ContentDecision::Allowed(CapSet::of(&[FS]).union(net)),
    );
    let document = Context::new(CapSet::ALL, "background project");
    let mut graph = f.graph();
    let id = graph.add_node(loose(&reader), [0.0, 0.0]);
    assert_eq!(
        read_through(&mut graph, id, Some(&document)).as_deref(),
        Some(SECRET)
    );

    // A worker the runtime started, still running while its tab is in the
    // background (its graph is not evaluated from now on).
    let context = runtime_context(&graph, id);
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker = {
        let _entered = context.enter();
        crate::capability::spawn(move || {
            while go_rx.recv().is_ok() {
                result_tx
                    .send(crate::capability::check(FS).is_ok())
                    .unwrap();
            }
        })
    };
    go_tx.send(()).unwrap();
    assert!(result_rx.recv().unwrap());
    // The copy a tab keeps while another tab is shown.
    let mut parked = graph.clone();

    // The approval narrows (same content, no file access any more). The
    // editor applies trust changes to every open tab without showing it.
    f.record(&reader, ContentDecision::Allowed(net));
    graph.apply_trust_change();
    assert!(context.is_revoked(), "the old, wider rights end now");
    assert!(
        graph.has_custom_runtime(id),
        "the same content keeps its runtime"
    );
    go_tx.send(()).unwrap();
    assert!(!result_rx.recv().unwrap(), "the worker lost file access");

    // The file is denied: runtimes go in every tab, shown or not.
    let digest = custom_node_digest(&std::fs::read_to_string(&reader).unwrap()).unwrap();
    f.store
        .content_ledger()
        .decide_file(ContentKind::CustomNode, &reader, &digest, false)
        .unwrap();
    graph.apply_trust_change();
    parked.apply_trust_change();
    assert!(!graph.has_custom_runtime(id));
    assert!(!parked.has_custom_runtime(id));
    // Shown again later: it does not run.
    assert_eq!(read_through(&mut parked, id, Some(&document)), None);
    drop(go_tx);
    worker.join().unwrap();
}

// ── P4.3.2F: final security regression ───────────────────────────────────

#[test]
fn network_and_device_operations_stop_once_rights_are_revoked() {
    let _enforce = Enforce::new();
    let fixture = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    fixture
        .set_read_timeout(Some(std::time::Duration::from_millis(400)))
        .unwrap();
    let port = fixture.local_addr().unwrap().port();
    let send = |node_id| crate::osc::OscAction::Send {
        node_id,
        host: "127.0.0.1".into(),
        port,
        address: "/patchwork/test".into(),
        args: vec![1.0],
    };
    let received = || {
        let mut buffer = [0u8; 256];
        fixture.recv(&mut buffer).is_ok()
    };
    let mut osc = crate::osc::OscManager::new();
    let mut serial = crate::serial::SerialManager::new();
    let document = Context::new(CapSet::ALL, "running project");
    {
        let _entered = document.enter();
        osc.process(vec![send(1)]);
    }
    assert!(received(), "a running project sends");

    // Revoked during execution (closed, denied, narrowed).
    document.revoke();
    {
        let _entered = document.enter();
        // Another node (identical repeats are not re-sent anyway).
        osc.process(vec![send(3)]);
        serial.process(vec![crate::serial::SerialAction::Connect {
            node_id: 2,
            port_name: "PATCHWORK-NO-SUCH-PORT".into(),
            baud_rate: 9600,
        }]);
        let refused = crate::capability::check(Capability::Network).unwrap_err();
        assert!(refused.revoked, "reported as withdrawn");
    }
    assert!(!received(), "nothing is sent after revocation");
    assert!(!serial.is_connected(2), "no device is opened");
    assert!(document.denied_count(Capability::Network) >= 1);
    assert!(document.denied_count(Capability::Devices) >= 1);
    assert_eq!(document.allowed_count(Capability::Devices), 0);
}
