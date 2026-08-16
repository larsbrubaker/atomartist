//! Unit tests for [`crate::eval_errors`] — the "post it once" rule that
//! keeps a permanently broken graph from flooding the status bar, and the
//! silent-recovery rule that keeps a fixed node from bragging about it.

use super::*;
use atomartist_lib::graph::executor::NodeFailure;

fn errors() -> NodeErrors {
    Arc::new(Mutex::new(HashMap::new()))
}

fn notices() -> Notices {
    Arc::new(Mutex::new(Vec::new()))
}

fn one(node: u64, message: &str) -> HashMap<NodeId, String> {
    HashMap::from([(NodeId(node), message.to_string())])
}

/// Every node id these tests mention, so nothing is pruned as deleted
/// except where a test says so.
fn live() -> HashSet<NodeId> {
    (0..20).map(NodeId).collect()
}

/// One pass: `failures` failed, `succeeded` came back clean, and the
/// graph still holds every node in [`live`].
fn pass<'a>(
    failures: HashMap<NodeId, String>,
    succeeded: &'a [NodeId],
    live: &'a HashSet<NodeId>,
) -> PassOutcome<'a> {
    PassOutcome {
        failures,
        warnings: HashMap::new(),
        succeeded,
        live,
    }
}

/// One pass whose only news is a warning: the node evaluated fine.
fn warning_pass<'a>(
    warnings: HashMap<NodeId, String>,
    succeeded: &'a [NodeId],
    live: &'a HashSet<NodeId>,
) -> PassOutcome<'a> {
    PassOutcome {
        failures: HashMap::new(),
        warnings,
        succeeded,
        live,
    }
}

/// The evaluator runs on every drag sample. A graph that stays broken
/// must say so exactly once, no matter how many passes see it — and the
/// notice queue being drained between passes (which it is, once a frame)
/// must not resurrect the message.
#[test]
fn a_persistently_failing_node_reports_once_not_every_pass() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    let mut posted = Vec::new();
    for _ in 0..5 {
        record(
            &errors,
            &warnings,
            &notices,
            pass(
                one(1, "Boolean: input 'b' is not a closed solid"),
                &[],
                &live,
            ),
        );
        // Stand in for the UI's once-per-frame drain, so the tail-
        // duplicate check inside `push_notice` cannot be what saves us.
        posted.extend(notices.lock().unwrap().drain(..));
    }

    assert_eq!(posted.len(), 1, "five failing passes, one message");
    assert_eq!(posted[0].level, NoticeLevel::Error);
}

/// A *different* failure on the same node is news — the user changed
/// something and hit a new wall.
#[test]
fn a_changed_message_on_the_same_node_is_reported_again() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );
    record(
        &errors,
        &warnings,
        &notices,
        pass(
            one(1, "Boolean: input 'b' is not a closed solid"),
            &[],
            &live,
        ),
    );

    assert_eq!(notices.lock().unwrap().len(), 2);
}

/// Recovery is silent: the badge clears, the status bar says nothing.
#[test]
fn a_fixed_node_clears_its_error_without_posting() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );
    notices.lock().unwrap().clear();

    // The repaired node re-evaluated cleanly this pass.
    record(
        &errors,
        &warnings,
        &notices,
        pass(HashMap::new(), &[NodeId(1)], &live),
    );

    assert!(errors.lock().unwrap().is_empty(), "the badge clears");
    assert!(
        notices.lock().unwrap().is_empty(),
        "no 'fixed!' message follows a repair"
    );
}

/// The same node failing again *after* it was fixed is news again.
#[test]
fn a_node_that_breaks_again_after_a_repair_reports_again() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    let mut posted = Vec::new();
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );
    // The UI drains once a frame; without that, `push_notice`'s own
    // tail-duplicate check would hide the repeat and this test would
    // pass for the wrong reason.
    posted.extend(notices.lock().unwrap().drain(..));
    record(
        &errors,
        &warnings,
        &notices,
        pass(HashMap::new(), &[NodeId(1)], &live),
    );
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );
    posted.extend(notices.lock().unwrap().drain(..));

    assert_eq!(posted.len(), 2);
}

/// `evaluate_dirty` walks only part of the graph. A pass that never
/// touched the broken node must leave its badge alone — otherwise
/// editing an unrelated node would "fix" the Boolean on screen.
#[test]
fn a_pass_that_did_not_touch_the_broken_node_keeps_its_error() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );

    // Some unrelated node re-evaluated; the broken one was not in the
    // dirty set at all.
    record(
        &errors,
        &warnings,
        &notices,
        pass(HashMap::new(), &[NodeId(9)], &live),
    );

    assert_eq!(
        errors.lock().unwrap().get(&NodeId(1)).map(String::as_str),
        Some("Boolean: input 'b' is empty")
    );
}

/// Deleting a broken node takes its error with it. Nothing else would:
/// a deleted node never evaluates successfully, so the "succeeded"
/// route can't clear it.
#[test]
fn deleting_a_broken_node_drops_its_error() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    record(
        &errors,
        &warnings,
        &notices,
        pass(one(1, "Boolean: input 'b' is empty"), &[], &live),
    );

    let without_node_1: HashSet<NodeId> = live.iter().copied().filter(|n| n.0 != 1).collect();
    record(
        &errors,
        &warnings,
        &notices,
        pass(HashMap::new(), &[], &without_node_1),
    );

    assert!(
        errors.lock().unwrap().is_empty(),
        "a deleted node cannot keep a badge"
    );
}

/// Two nodes failing with the *same* sentence in one pass badge both
/// nodes but say it once — `push_notice` drops the consecutive
/// duplicate, and repeating one line in a one-line status bar tells the
/// user nothing. Pinned because it is behaviour we rely on, not an
/// accident of the queue.
#[test]
fn two_nodes_failing_with_the_same_text_say_it_once() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    let mut failures = one(1, "Boolean: input 'b' is not a closed solid");
    failures.insert(
        NodeId(2),
        "Boolean: input 'b' is not a closed solid".to_string(),
    );

    record(&errors, &warnings, &notices, pass(failures, &[], &live));

    assert_eq!(notices.lock().unwrap().len(), 1, "one sentence, once");
    assert_eq!(errors.lock().unwrap().len(), 2, "but both nodes badged");
}

/// A warning posts at [`NoticeLevel::Warning`] and — the part that
/// matters — never lands in the error map, which is what the canvas
/// badges. A degraded Boolean is not a broken one.
#[test]
fn a_warning_is_posted_but_never_badged() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());

    record(
        &errors,
        &warnings,
        &notices,
        warning_pass(
            one(1, "Boolean: 1 of 3 parts are not watertight solids"),
            &[NodeId(1)],
            &live,
        ),
    );

    let posted = notices.lock().unwrap().clone();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].level, NoticeLevel::Warning);
    assert!(errors.lock().unwrap().is_empty(), "no badge for a warning");
    assert_eq!(warnings.lock().unwrap().len(), 1);
}

/// The same "say it once" rule as errors: a graph parked on a degraded
/// Boolean must not repeat itself every pass.
#[test]
fn a_repeated_warning_is_only_said_once() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    let mut posted = Vec::new();
    for _ in 0..5 {
        record(
            &errors,
            &warnings,
            &notices,
            warning_pass(
                one(1, "Boolean: 1 of 3 parts are not watertight"),
                &[NodeId(1)],
                &live,
            ),
        );
        posted.extend(notices.lock().unwrap().drain(..));
    }
    assert_eq!(posted.len(), 1, "five degraded passes, one message");
}

/// A node that stops warning drops its message silently — the same
/// silent-recovery rule the errors follow.
#[test]
fn a_node_that_stops_warning_clears_silently() {
    let (errors, warnings, notices, live) = (errors(), errors(), notices(), live());
    record(
        &errors,
        &warnings,
        &notices,
        warning_pass(
            one(1, "Boolean: 1 of 3 parts are not watertight"),
            &[NodeId(1)],
            &live,
        ),
    );
    notices.lock().unwrap().clear();

    record(
        &errors,
        &warnings,
        &notices,
        warning_pass(HashMap::new(), &[NodeId(1)], &live),
    );

    assert!(warnings.lock().unwrap().is_empty());
    assert!(notices.lock().unwrap().is_empty(), "recovery is silent");
}

/// The node type's display name prefixes a warning too — but only once,
/// because the Boolean node's own messages already start with "Boolean:".
#[test]
fn a_warning_is_not_double_prefixed() {
    let mut registry = atomartist_lib::registry::NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);
    let report = atomartist_lib::graph::executor::EvalReport {
        walked: Vec::new(),
        skipped: Vec::new(),
        failures: Vec::new(),
        warnings: vec![atomartist_lib::graph::executor::NodeWarning {
            node: NodeId(3),
            type_id: "Boolean".into(),
            message: "Boolean: 1 of 3 parts are not watertight solids".into(),
        }],
    };

    let messages = warnings_for(&report, &registry);

    let text = messages.get(&NodeId(3)).expect("the warning is described");
    assert_eq!(text, "Boolean: 1 of 3 parts are not watertight solids");
}

/// The message the user reads names the node type, then the node's own
/// (operand-naming) sentence.
#[test]
fn the_message_is_prefixed_with_the_node_type_display_name() {
    let mut registry = atomartist_lib::registry::NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);
    let report = atomartist_lib::graph::executor::EvalReport {
        walked: Vec::new(),
        skipped: Vec::new(),
        failures: vec![NodeFailure {
            node: NodeId(3),
            type_id: "Boolean".into(),
            message: "input 'b' is not a closed solid".into(),
        }],
        warnings: Vec::new(),
    };

    let messages = messages_for(&report, &registry);

    let text = messages.get(&NodeId(3)).expect("the failure is described");
    assert!(
        text.ends_with("input 'b' is not a closed solid"),
        "{}",
        text
    );
    assert!(text.starts_with("Boolean"), "{}", text);
}
