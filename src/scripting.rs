//! P4.3.2B: hardened Rhai engines for script-bearing nodes.
//!
//! Every Rhai engine that runs code from a graph (the Script node and the
//! Math formula node) is built here, so the limits are applied in one
//! place:
//!
//! * no module imports (no file-system module resolver, zero modules);
//! * `eval` disabled (no dynamic code construction);
//! * finite operations, wall-clock time, expression depth, call depth,
//!   string, array and map sizes;
//! * `print` / `debug` go to the system log instead of stdout;
//! * bounded source size and plain identifiers for injected variables.
//!
//! Limits make a hostile script fail with a node error instead of hanging
//! or exhausting memory. They are not a sandbox for CPU time spent within
//! the limits, nor for memory below the size limits: a script may still
//! use up to `max_time` of CPU on every evaluation.

use std::time::{Duration, Instant};

/// Limits for one evaluation.
#[derive(Debug, Clone, Copy)]
pub struct ScriptLimits {
    pub max_operations: u64,
    pub max_time: Duration,
    pub max_expr_depth: usize,
    pub max_function_expr_depth: usize,
    pub max_call_levels: usize,
    pub max_string_size: usize,
    pub max_array_size: usize,
    pub max_map_size: usize,
    /// Largest accepted source text, in bytes.
    pub max_source_bytes: usize,
}

/// Script node: runs every frame, so a single run must stay short.
pub const SCRIPT_LIMITS: ScriptLimits = ScriptLimits {
    max_operations: 500_000,
    max_time: Duration::from_millis(50),
    max_expr_depth: 64,
    max_function_expr_depth: 32,
    max_call_levels: 32,
    max_string_size: 64 * 1024,
    max_array_size: 10_000,
    max_map_size: 10_000,
    max_source_bytes: 64 * 1024,
};

/// Math formula node: a single expression.
pub const FORMULA_LIMITS: ScriptLimits = ScriptLimits {
    max_operations: 10_000,
    max_time: Duration::from_millis(10),
    max_expr_depth: 64,
    max_function_expr_depth: 16,
    max_call_levels: 16,
    max_string_size: 4 * 1024,
    max_array_size: 1_000,
    max_map_size: 1_000,
    max_source_bytes: 4 * 1024,
};

/// An engine with Rhai's standard functions but none of the escape
/// hatches. The time limit starts counting when the engine is built, so
/// build one per evaluation.
pub fn sandboxed_engine(limits: &ScriptLimits) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.set_max_modules(0);
    engine.disable_symbol("eval");
    engine.set_max_operations(limits.max_operations);
    engine.set_max_expr_depths(limits.max_expr_depth, limits.max_function_expr_depth);
    engine.set_max_call_levels(limits.max_call_levels);
    engine.set_max_string_size(limits.max_string_size);
    engine.set_max_array_size(limits.max_array_size);
    engine.set_max_map_size(limits.max_map_size);
    let deadline = Instant::now() + limits.max_time;
    engine.on_progress(move |_| {
        (Instant::now() > deadline).then(|| rhai::Dynamic::from("time limit exceeded".to_string()))
    });
    engine.on_print(|text| crate::system_log::log(format!("Script: {text}")));
    engine.on_debug(|text, _, _| crate::system_log::log(format!("Script debug: {text}")));
    engine
}

/// Source text within the limit.
pub fn check_source(source: &str, limits: &ScriptLimits) -> Result<(), String> {
    if source.len() > limits.max_source_bytes {
        return Err(format!(
            "Script is too long ({} bytes; the limit is {}).",
            source.len(),
            limits.max_source_bytes
        ));
    }
    Ok(())
}

/// A variable name injected into generated code: `[A-Za-z_][A-Za-z0-9_]*`,
/// at most 64 characters, so it can never smuggle in code.
pub fn check_identifier(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("'{name}' is not a valid variable name."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: &str) -> Result<rhai::Dynamic, String> {
        sandboxed_engine(&SCRIPT_LIMITS)
            .eval::<rhai::Dynamic>(code)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn ordinary_scripts_still_work() {
        assert_eq!(run("let x = 2; x * 21").unwrap().as_int().unwrap(), 42);
        assert_eq!(run("fn f(a) { a + 1 } f(1)").unwrap().as_int().unwrap(), 2);
        assert!((run("sin(0.0)").unwrap().as_float().unwrap()).abs() < 1e-9);
    }

    #[test]
    fn runaway_scripts_stop() {
        let started = Instant::now();
        assert!(run("loop {}").is_err(), "infinite loop");
        assert!(run("let x = 0; while true { x += 1; }").is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stopped promptly"
        );
        assert!(
            run("fn f(n) { f(n + 1) } f(0)").is_err(),
            "unbounded recursion"
        );
        assert!(
            run("let s = \"x\"; loop { s += s; }").is_err(),
            "string growth"
        );
        assert!(
            run("let a = []; loop { a.push(1); }").is_err(),
            "array growth"
        );
        let deep = format!("{}1{}", "(".repeat(500), ")".repeat(500));
        assert!(run(&deep).is_err(), "expression depth");
    }

    #[test]
    fn the_time_limit_applies_even_below_the_operation_limit() {
        let mut limits = SCRIPT_LIMITS;
        limits.max_operations = 0; // unlimited operations
        limits.max_time = Duration::from_millis(20);
        let started = Instant::now();
        let result = sandboxed_engine(&limits).eval::<rhai::Dynamic>("loop {}");
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn imports_and_eval_are_disabled() {
        let tmp = std::env::temp_dir().join("p432b_module.rhai");
        std::fs::write(&tmp, "fn secret() { 42 }").unwrap();
        let path = tmp.to_string_lossy().replace('\\', "/");
        let error = run(&format!("import \"{path}\" as m; m::secret()")).unwrap_err();
        assert!(!error.is_empty());
        assert!(run("import \"p432b_module\" as m; 1").is_err());
        assert!(run("eval(\"40 + 2\")").is_err());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn source_and_identifier_checks() {
        assert!(check_source(&"x".repeat(SCRIPT_LIMITS.max_source_bytes), &SCRIPT_LIMITS).is_ok());
        assert!(
            check_source(
                &"x".repeat(SCRIPT_LIMITS.max_source_bytes + 1),
                &SCRIPT_LIMITS
            )
            .is_err()
        );
        for good in ["a", "_x", "out_1", "Gain"] {
            assert!(check_identifier(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "1a",
            "a b",
            "a; import \"x\" as m",
            "a=1",
            "ä",
            &"a".repeat(65),
        ] {
            assert!(check_identifier(bad).is_err(), "{bad}");
        }
    }

    /// The Script node through real graph evaluation.
    fn script_node(code: &str, outputs: &[&str]) -> (crate::graph::Graph, crate::graph::NodeId) {
        let node: crate::graph::NodeType = serde_json::from_value(serde_json::json!({"Script": {
            "name": "S", "input_names": [], "output_names": outputs, "code": code, "continuous": true
        }}))
        .unwrap();
        let mut graph = crate::graph::Graph::new();
        let id = graph.add_node(node, [0.0, 0.0]);
        (graph, id)
    }

    fn script_error(graph: &crate::graph::Graph, id: crate::graph::NodeId) -> String {
        match &graph.nodes[&id].node_type {
            crate::graph::NodeType::Script { error, .. } => error.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn script_nodes_run_within_limits_and_report_errors() {
        let (mut graph, id) = script_node("y = 6.0 * 7.0;", &["y"]);
        let values = graph.evaluate(0.0);
        assert!(matches!(
            values.get(&(id, 0)),
            Some(crate::graph::PortValue::Float(v)) if (*v - 42.0).abs() < 1e-6
        ));
        assert!(script_error(&graph, id).is_empty());

        let started = Instant::now();
        let (mut graph, id) = script_node("loop {}", &["y"]);
        let values = graph.evaluate(0.0);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the frame was not blocked"
        );
        assert!(values.get(&(id, 0)).is_none());
        assert!(
            !script_error(&graph, id).is_empty(),
            "a controlled node error"
        );

        let (mut graph, id) = script_node("import \"secrets\" as s; y = 1.0;", &["y"]);
        graph.evaluate(0.0);
        assert!(!script_error(&graph, id).is_empty(), "imports are disabled");

        let (mut graph, id) = script_node("y = 1.0;", &["y; import \"x\" as m"]);
        graph.evaluate(0.0);
        assert!(script_error(&graph, id).contains("not a valid variable name"));

        let (mut graph, id) = script_node(&"// x\n".repeat(20_000), &["y"]);
        graph.evaluate(0.0);
        assert!(script_error(&graph, id).contains("too long"));
    }
}
