// src/route_ledger_tests.rs
//
// End-to-end tests of the ledger write-order rule through Route::run.
// Kept in their own file (wired from main.rs) so they don't depend on the
// layout of route.rs's own test module.

use std::path::Path;

use crate::envelope::{Envelope, Payload, Source};
use crate::event::EventKind;
use crate::route::{Hop, Route, Stop};

fn ledger_records(vault: &Path) -> Vec<crate::ledger::Record> {
    crate::ledger::for_vault(vault).unwrap().records().unwrap()
}

// sha256("hello") is a published test vector:
// 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
#[tokio::test]
async fn the_ledger_records_received_arrival_then_completion_carrying_the_content_hash() {
    let vault = tempfile::tempdir().unwrap();
    let envelope = Envelope::new(
        Source::Manual,
        "text/note",
        Payload::Text("hello".into()),
        Some("34-B"),
    );
    let mut route = Route::new(
        "test-dodo",
        vec![Stop {
            nest_id: "34-B".to_string(),
            hops: vec![],
        }],
    );

    route.run(vault.path(), Some(&envelope)).await.unwrap();

    let r = ledger_records(vault.path());
    let hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    assert_eq!(r.len(), 3);
    assert_eq!((r[0].seq, r[0].event.kind), (1, EventKind::Received));
    assert_eq!((r[1].seq, r[1].event.kind), (2, EventKind::Transition));
    assert_eq!((r[2].seq, r[2].event.kind), (3, EventKind::Completed));
    assert_eq!(
        r[2].event.caused_by,
        Some(r[1].event.event_id),
        "completion points at its arrival"
    );
    let facts = r[0]
        .event
        .envelope
        .as_ref()
        .expect("Received carries the envelope's facts");
    assert_eq!(facts.node, format!("env:{}", envelope.id));
    assert_eq!(facts.data_type, "text/note");
    for rec in &r {
        assert_eq!(rec.event.envelope_id, Some(envelope.id));
        assert_eq!(rec.event.content_hash.as_deref(), Some(hello));
    }
    assert!(crate::ledger::check(&r).interrupted.is_empty());
}

#[tokio::test]
async fn a_failing_stop_leaves_arrival_then_error_in_the_ledger_and_no_completion() {
    let vault = tempfile::tempdir().unwrap();
    let mut route = Route::new(
        "test-dodo",
        vec![
            Stop {
                nest_id: "22".to_string(),
                hops: vec![Hop::RunScript {
                    command: "false".into(),
                    args: vec![],
                }],
            },
            Stop {
                nest_id: "52".to_string(),
                hops: vec![],
            },
        ],
    );

    assert!(route.run(vault.path(), None).await.is_err());

    let kinds: Vec<EventKind> = ledger_records(vault.path())
        .iter()
        .map(|r| r.event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![EventKind::Transition, EventKind::Error],
        "stop 52 is never reached"
    );
    let r = ledger_records(vault.path());
    assert_eq!(r[1].event.caused_by, Some(r[0].event.event_id));
    assert!(
        crate::ledger::check(&r).interrupted.is_empty(),
        "a recorded failure is not an interruption"
    );
}

#[tokio::test]
async fn an_emitted_childs_events_name_the_parent_envelope() {
    let vault = tempfile::tempdir().unwrap();
    let parent = Envelope::new(
        Source::Manual,
        "text/journal",
        Payload::Text("lunch".into()),
        Some("34-B"),
    );
    let items = r#"[{"data_type":"data/food-intake","payload":{"kind":"text","data":"burrito"}}]"#;
    let mut route = Route::new(
        "test-dodo",
        vec![Stop {
            nest_id: "34-B".to_string(),
            hops: vec![
                Hop::RunScript {
                    command: "sh".into(),
                    args: vec!["-c".into(), format!("echo '{items}'")],
                },
                Hop::Emit,
            ],
        }],
    );
    route.run(vault.path(), Some(&parent)).await.unwrap();
    let child = route.emitted.pop().unwrap();

    let mut child_route = Route::new(
        "test-dodo",
        vec![Stop {
            nest_id: "35-A".to_string(),
            hops: vec![],
        }],
    );
    child_route.run(vault.path(), Some(&child)).await.unwrap();

    let r = ledger_records(vault.path());
    assert_eq!(
        r.len(),
        6,
        "for each of parent and child: received, arrival, completion"
    );
    assert!(r[..3].iter().all(|x| x.event.envelope_parent.is_none()));
    assert!(r[3..]
        .iter()
        .all(|x| x.event.envelope_parent == Some(parent.id)));
    assert!(
        r[3].event.content_hash.is_some() && r[3].event.content_hash != r[0].event.content_hash
    );
    let health = crate::ledger::check(&r);
    assert!(
        health.dangling_parents.is_empty(),
        "the parent was recorded first"
    );
}

#[tokio::test]
async fn concurrent_routes_on_one_vault_share_one_gapless_ledger() {
    let vault = tempfile::tempdir().unwrap();
    let make = |nest: &str| {
        Route::new(
            "test-dodo",
            vec![
                Stop {
                    nest_id: nest.to_string(),
                    hops: vec![],
                },
                Stop {
                    nest_id: format!("{nest}-next"),
                    hops: vec![],
                },
            ],
        )
    };
    let (mut a, mut b, mut c) = (make("a"), make("b"), make("c"));

    let (ra, rb, rc) = tokio::join!(
        a.run(vault.path(), None),
        b.run(vault.path(), None),
        c.run(vault.path(), None),
    );
    ra.unwrap();
    rb.unwrap();
    rc.unwrap();

    let seqs: Vec<u64> = ledger_records(vault.path()).iter().map(|r| r.seq).collect();
    assert_eq!(
        seqs,
        (1..=12).collect::<Vec<u64>>(),
        "3 routes x 2 stops x (arrival + completion)"
    );
}
