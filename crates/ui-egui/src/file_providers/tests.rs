#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{Capture, Command, Host, Resource};
use serde_json::json;

#[test]
fn disconnect_clears_published_document_metadata_before_another_account_connects() {
    let (host, _) = connected();
    let documents = json!([{"id":"42", "title":"Private account A document", "dirty":true}]);
    host.publish_documents(documents.clone(), &[42]);
    assert_eq!(host.snapshot()["documents"], documents, "the regression must start with nonempty metadata");
    host.disconnect().unwrap();
    assert!(host.snapshot()["documents"].is_null());
    host.connect("test".into(), "Test files".into(), "account-two".into()).unwrap();
    let snapshot = host.snapshot();
    assert_eq!(snapshot["namespace"], "account-two");
    assert!(snapshot["documents"].is_null(), "the new account has not received an editor publication");
}

fn connected() -> (Host, String) {
    let host = Host::default();
    host.set_session("0123456789abcdef0123456789abcdef".into()).unwrap();
    let connection = host.connect("test".into(), "Test files".into(), "account-one".into()).unwrap();
    (host, connection)
}

fn request(host: &Host, doc: u64) -> String {
    host.choose(doc, 1, "Library.zip".into(), false).unwrap();
    host.take_events().remove(0).metadata["requestId"].as_str().unwrap().into()
}

fn capture(doc: u64, revision: u64) -> Capture {
    Capture { doc, revision, name: "Library.zip".into(), save_as: false }
}

#[test]
fn disabled_until_explicit_registration() {
    let host = Host::default();
    assert!(!host.enabled());
    assert!(!host.connected());
    assert!(host.choose(1, 1, "Library.zip".into(), false).is_err());
    assert!(host.request_open("one").is_err());
    assert!(host.take_events().is_empty());
}

#[test]
fn save_choice_does_not_capture_or_acknowledge() {
    let (host, _) = connected();
    let id = request(&host, 1);
    assert!(host.pending_for(1));
    assert!(host.result(id.clone(), "saved".into(), "one".into()).is_err());
    host.resolve_dialog(&id, "cancel", "").unwrap();
    assert!(!host.busy());
    assert!(!host.bound(1));
}

#[test]
fn repeated_save_has_one_destination_choice() {
    let (host, _) = connected();
    host.choose(1, 1, "Library.zip".into(), false).unwrap();
    host.choose(1, 2, "Library.zip".into(), false).unwrap();
    assert_eq!(host.take_events().len(), 1);
}

#[test]
fn exact_capture_ack_is_consumed_once_and_never_mutates_document() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    assert!(matches!(host.take_commands().remove(0), Command::Save { doc: 1, .. }));
    host.capture(&id, capture(1, 23), vec![1, 2, 3]).unwrap();
    let event = host.take_events().remove(0);
    assert_eq!(event.bytes, Some(vec![1, 2, 3]));
    assert_eq!(event.metadata["revision"], 23);
    host.result(id.clone(), "saved".into(), "one".into()).unwrap();
    assert!(host.result(id, "saved".into(), "one".into()).is_err());
    assert!(matches!(host.take_commands().remove(0), Command::Result { capture: Capture { doc: 1, revision: 23, .. }, .. }));
    assert!(!host.bound(1), "only the application admits a durable result");
}

#[test]
fn failure_cancel_and_account_changes_do_not_rebind() {
    let (host, _) = connected();
    let id = request(&host, 1);
    assert!(host.disconnect().is_err());
    assert!(host.connect("test".into(), "Test files".into(), "other-account".into()).is_err());
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 1), vec![]).unwrap();
    host.result(id, "failed".into(), String::new()).unwrap();
    assert!(host.disconnect().is_err(), "queued results still belong to the old identity");
    host.take_commands();
    host.disconnect().unwrap();
    assert!(!host.bound(1));
}

#[test]
fn new_connection_rejects_old_list_and_read_result() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "Library.zip".into(), updated_at: String::new() }]).unwrap();
    let read = host.request_open("one").unwrap();
    host.cancel_open(read.clone(), String::new()).unwrap();
    host.take_events();
    host.disconnect().unwrap();
    let next = host.connect("test".into(), "Test files".into(), "account-one".into()).unwrap();
    assert_ne!(connection, next);
    assert!(host.set_resources(&connection, vec![]).is_err());
    assert!(host.open(read, "Library.zip".into(), vec![]).is_err());
}

#[test]
fn same_resource_alias_is_invalidated_but_homonymous_files_are_not() {
    let (host, _) = connected();
    host.bind(&capture(1, 1), "one".into());
    host.bind(&capture(2, 1), "two".into());
    assert_eq!(host.bind(&capture(3, 1), "one".into()), vec![1]);
    assert!(!host.bound(1));
    assert!(host.bound(2));
    assert!(host.bound(3));
}

#[test]
fn save_as_never_overwrites_binding_by_filename() {
    let (host, _) = connected();
    host.bind(&capture(1, 1), "one".into());
    host.choose(1, 1, "Library.zip".into(), true).unwrap();
    let dialog = host.take_events().remove(0).metadata;
    assert_eq!(dialog["saveAs"], true);
    assert!(dialog["resourceId"].is_null());
    let id = dialog["requestId"].as_str().unwrap().to_owned();
    host.resolve_dialog(&id, "provider", "").unwrap();
    let Command::Save { request: resolved, doc, destination, name, save_as } = host.take_commands().remove(0) else {
        panic!("expected the resolved Save As command");
    };
    assert_eq!(resolved, id);
    assert_eq!(doc, 1);
    assert_eq!(destination, "provider");
    assert!(save_as);
    host.capture(&id, Capture { doc, revision: 2, name, save_as }, vec![1, 2, 3]).unwrap();
    let event = host.take_events().remove(0);
    assert_eq!(event.metadata["type"], "save");
    assert!(event.metadata["resourceId"].is_null());
    assert_eq!(event.bytes, Some(vec![1, 2, 3]));
    host.result(id, "saved".into(), "two".into()).unwrap();
    let Command::Result { capture, resource, status } = host.take_commands().remove(0) else {
        panic!("expected the acknowledged Save As capture");
    };
    assert!(capture.save_as);
    assert_eq!((capture.doc, capture.revision), (1, 2));
    assert_eq!(resource, "two");
    assert_eq!(status, "saved");
    host.bind(&capture, resource);
    let resources = host.snapshot()["resources"].clone();
    assert!(resources.as_array().unwrap().iter().any(|r| r["id"] == "one"), "Save As leaves the old resource intact");
    let next = request(&host, 1);
    assert_eq!(host.state().bindings.get(&1).unwrap().resource, "two");
    host.resolve_dialog(&next, "cancel", "").unwrap();
}

#[test]
fn provider_open_is_correlated_and_committed_after_decode() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "Library.zip".into(), updated_at: String::new() }]).unwrap();
    let read = host.request_open("one").unwrap();
    assert!(host.open("unknown".into(), "Library.zip".into(), vec![]).is_err());
    host.open(read.clone(), "Library.zip".into(), vec![7]).unwrap();
    assert!(!host.bound(1));
    assert!(host.disconnect().is_err());
    let commands = host.take_commands();
    assert!(matches!(&commands[0], Command::Open { request, resource, bytes, .. } if request == &read && resource == "one" && bytes == &[7]));
    host.opened(&read, Some(capture(1, 1)), "");
    assert!(host.bound(1));
    assert!(host.open(read, "Library.zip".into(), vec![]).is_err());
}

#[test]
fn open_preflight_rejects_invalid_requests_and_payloads_without_consuming_the_read() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "Library.zip".into(), updated_at: String::new() }]).unwrap();
    let read = host.request_open("one").unwrap();
    host.take_events();
    assert!(host.check_open("unknown", "Library.zip", 0).is_err());
    assert!(host.check_open(&read, "../Library.zip", 0).is_err());
    assert!(host.check_open(&read, "Library.zip", super::MAX_BYTES + 1).is_err());
    host.check_open(&read, "Library.zip", 1).unwrap();
    assert!(host.take_commands().is_empty(), "preflight never admits or consumes the read");
    host.open(read.clone(), "Library.zip".into(), vec![7]).unwrap();
    assert!(host.check_open(&read, "Library.zip", 1).is_err());
    assert!(host.open(read, "Library.zip".into(), vec![7]).is_err());
}

#[test]
fn open_preflight_counts_queued_bytes_and_open_revalidates_command_capacity() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "Library.zip".into(), updated_at: String::new() }]).unwrap();
    let first = host.request_open("one").unwrap();
    let second = host.request_open("one").unwrap();
    host.take_events();
    host.open(first.clone(), "Library.zip".into(), vec![7]).unwrap();
    assert!(host.check_open(&second, "Library.zip", super::MAX_BYTES).is_err(), "the existing byte counts against the aggregate cap");
    host.check_open(&second, "Library.zip", 1).unwrap();
    host.take_commands();
    host.opened(&first, None, "test cancelled");
    host.take_events();
    for doc in 1..=super::MAX_PENDING as u64 {
        let id = request(&host, doc);
        host.resolve_dialog(&id, "provider", "").unwrap();
    }
    assert!(host.check_open(&second, "Library.zip", 1).is_err());
    assert!(host.open(second.clone(), "Library.zip".into(), vec![8]).is_err(), "open rechecks capacity after a successful preflight");
    host.take_commands();
    host.open(second, "Library.zip".into(), vec![8]).unwrap();
    assert!(matches!(host.take_commands().remove(0), Command::Open { bytes, .. } if bytes == [8]));
}

#[test]
fn metadata_limits_are_atomic() {
    let (host, connection) = connected();
    let resource = Resource { id: "one".into(), name: "Library.zip".into(), updated_at: String::new() };
    host.set_resources(&connection, vec![resource.clone()]).unwrap();
    assert!(host.set_resources(&connection, vec![resource.clone(), resource]).is_err());
    assert_eq!(host.snapshot()["resources"].as_array().unwrap().len(), 1);
    assert!(host.choose(1, 1, "../outside.docx".into(), false).is_err());
    assert!(host.connect("bad/id".into(), "Label".into(), "account".into()).is_err());
}

#[test]
fn closed_documents_retire_unresolved_choices() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.publish_documents(json!([]), &[]);
    assert!(host.resolve_dialog(&id, "provider", "").is_err());
    assert!(!host.busy());
}

#[test]
fn closed_document_keeps_its_capture_result_reachable_without_binding_the_new_document() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 23), vec![1, 2, 3]).unwrap();
    assert_eq!(host.take_events().remove(0).metadata["type"], "save");
    host.bind(&capture(2, 7), "two".into());
    let documents = json!([{"documentId":"2", "revision":7}]);
    host.publish_documents(documents.clone(), &[2]);
    assert!(host.pending_for(1), "the closed document still owns the already transported capture");
    host.result(id.clone(), "saved".into(), "one".into()).unwrap();
    assert!(host.result(id, "saved".into(), "one".into()).is_err(), "a stale or duplicate ACK cannot be consumed again");
    let Command::Result { capture, resource, status } = host.take_commands().remove(0) else {
        panic!("expected the closed document's acknowledged capture");
    };
    assert_eq!((capture.doc, capture.revision), (1, 23), "the result must not be retargeted to the new document");
    assert_eq!(resource, "one");
    assert_eq!(status, "saved");
    assert_eq!(host.snapshot()["documents"], documents);
    assert!(!host.bound(1), "consuming a Host result does not itself bind the closed document; this test does not run WebApp::provider_tick");
    assert_eq!(host.state().bindings.get(&2).unwrap().resource, "two");
    assert!(!host.busy());
}

#[test]
fn disabled_provider_keeps_pending_result_reachable() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 1), vec![]).unwrap();
    host.set_enabled(false).unwrap();
    host.result(id, "cancel".into(), String::new()).unwrap();
    host.take_commands();
    host.disconnect().unwrap();
}

#[test]
fn failed_save_delivery_becomes_a_correlated_failure_and_later_events_still_dispatch() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 23), vec![1, 2, 3]).unwrap();
    host.state().event(json!({"type":"after-save"}), None).unwrap();
    let mut errors = Vec::new();
    let mut delivered = Vec::new();
    let repaint = host.publish_events_with(
        |metadata, bytes| {
            if metadata["type"] == "save" {
                assert_eq!(metadata["requestId"], id);
                assert_eq!(bytes, Some(vec![1, 2, 3]));
                Err("recoverable JavaScript payload allocation failure")
            } else {
                delivered.push(metadata["type"].as_str().unwrap().to_string());
                Ok(())
            }
        },
        |error| errors.push(error),
    );
    assert!(repaint, "the app must process the failed result without another host callback");
    assert_eq!(errors, vec!["recoverable JavaScript payload allocation failure"]);
    assert_eq!(delivered, vec!["after-save"]);
    assert!(host.state().captures.is_empty(), "taken bytes cannot leave an unreachable capture");
    assert!(host.result(id, "saved".into(), "late".into()).is_err(), "the failed delivery cannot later acknowledge persistence");
    let commands = host.take_commands();
    assert_eq!(commands.len(), 1);
    let Command::Result { capture, resource, status } = &commands[0] else { panic!("expected correlated delivery failure") };
    assert_eq!((capture.doc, capture.revision), (1, 23));
    assert_eq!(status, "failed");
    assert!(resource.is_empty());
    assert!(!host.bound(1));
    assert!(!host.pending_for(1));
    assert!(!host.busy());
    host.disconnect().unwrap();
}

#[test]
fn successful_save_delivery_stays_pending_until_the_hosts_explicit_result() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 23), vec![4, 5, 6]).unwrap();
    let mut delivered = 0;
    let mut errors = 0;
    assert!(!host.publish_events_with(
        |metadata, bytes| {
            assert_eq!(metadata["requestId"], id);
            assert_eq!(bytes, Some(vec![4, 5, 6]));
            delivered += 1;
            Ok::<(), &str>(())
        },
        |_| errors += 1,
    ));
    assert_eq!((delivered, errors), (1, 0));
    assert!(host.pending_for(1));
    assert!(host.take_commands().is_empty(), "successful delivery is not a persistence acknowledgement");
    host.result(id, "saved".into(), "one".into()).unwrap();
    let commands = host.take_commands();
    assert!(matches!(commands.as_slice(), [Command::Result { status, .. }] if status == "saved"));
}

#[test]
fn failed_delivery_recovers_its_existing_capture_even_with_a_full_command_queue() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "Other.zip".into(), updated_at: String::new() }]).unwrap();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 23), vec![7]).unwrap();
    let mut reads = Vec::new();
    for _ in 0..super::MAX_PENDING {
        let read = host.request_open("one").unwrap();
        host.open(read.clone(), "Other.zip".into(), vec![]).unwrap();
        reads.push(read);
    }
    assert_eq!(host.state().commands.len(), super::MAX_PENDING);
    assert!(host.result(id.clone(), "saved".into(), "one".into()).unwrap_err().contains("queue is full"));
    assert!(host.publish_events_with(
        |metadata, _| if metadata["type"] == "save" { Err("delivery failed") } else { Ok(()) },
        |_| {},
    ));
    assert!(host.state().captures.is_empty());
    let commands = host.take_commands();
    assert_eq!(commands.len(), super::MAX_PENDING + 1);
    assert_eq!(commands.iter().filter(|command| matches!(command, Command::Open { .. })).count(), reads.len());
    let failures: Vec<_> = commands.iter().filter_map(|command| match command {
        Command::Result { capture, resource, status } => Some((capture.doc, capture.revision, resource.as_str(), status.as_str())),
        _ => None,
    }).collect();
    assert_eq!(failures, vec![(1, 23, "", "failed")]);
    assert!(!host.bound(1));
    assert!(host.result(id, "saved".into(), "late".into()).is_err());
    for read in reads {
        host.opened(&read, None, "fixture complete");
    }
    assert!(!host.publish_events_with(|_, _| Ok::<(), &str>(()), |_| {}));
    assert!(!host.busy());
}

#[test]
fn delivery_error_cannot_replace_a_result_already_accepted_from_a_synchronous_listener() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "provider", "").unwrap();
    host.take_commands();
    host.capture(&id, capture(1, 23), vec![8]).unwrap();
    assert!(host.publish_events_with(
        |metadata, _| {
            assert_eq!(metadata["requestId"], id);
            host.result(id.clone(), "saved".into(), "one".into()).unwrap();
            Err("dispatcher failed after the listener's result")
        },
        |_| {},
    ));
    let commands = host.take_commands();
    assert_eq!(commands.len(), 1);
    let Command::Result { capture, resource, status } = &commands[0] else { panic!("expected the listener's result") };
    assert_eq!((capture.doc, capture.revision, resource.as_str(), status.as_str()), (1, 23, "one", "saved"));
    assert!(!host.busy());
}

#[test]
fn taking_a_choice_retains_account_and_document_ownership_until_serialization_finishes() {
    let (host, _) = connected();
    let id = request(&host, 1);
    host.resolve_dialog(&id, "device", "").unwrap();
    host.take_commands();
    assert!(host.pending_for(1));
    assert!(host.disconnect().is_err());
    host.finish_device(&id);
    assert!(!host.busy());
    host.disconnect().unwrap();
}

fn fill_event_queue(host: &Host) {
    let mut state = host.state();
    for index in 0..super::MAX_EVENTS {
        state.event(json!({"type":"queued", "index":index}), None).unwrap();
    }
}

#[test]
fn full_event_queue_retains_open_success_and_failure_until_ordered_drain() {
    for success in [true, false] {
        let (host, connection) = connected();
        host.set_resources(&connection, vec![Resource { id: "one".into(), name: "One.zip".into(), updated_at: String::new() }]).unwrap();
        let read = host.request_open("one").unwrap();
        host.open(read.clone(), "One.zip".into(), vec![7]).unwrap();
        host.take_commands();
        host.take_events();
        fill_event_queue(&host);
        host.opened(&read, success.then(|| capture(1, 1)), "decoder failed\n");
        host.opened(&read, None, "duplicate");
        assert!(host.busy());
        assert!(host.disconnect().is_err());
        assert_eq!(host.state().finished.len(), 1);
        let earlier = host.take_events();
        assert_eq!(earlier.len(), super::MAX_EVENTS);
        assert!(earlier.iter().all(|event| event.metadata["type"] == "queued"));
        let terminal = host.take_events();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].metadata["requestId"], read);
        assert_eq!(terminal[0].metadata["resourceId"], "one");
        assert_eq!(terminal[0].metadata["namespace"], "account-one");
        assert_eq!(terminal[0].metadata["connectionId"], connection);
        assert_eq!(terminal[0].metadata["type"], if success { "opened" } else { "openFailed" });
        assert_eq!(host.bound(1), success);
        assert!(!host.busy());
        assert!(host.take_events().is_empty());
        host.disconnect().unwrap();
    }
}

#[test]
fn full_event_queue_retains_save_failure_and_rejects_unowned_or_duplicate_terminals() {
    let (host, _) = connected();
    let save = request(&host, 1);
    host.resolve_dialog(&save, "provider", "").unwrap();
    host.take_commands();
    fill_event_queue(&host);
    host.failed(&save, "encoder failed");
    host.failed(&save, "duplicate");
    host.failed("unowned", "forged");
    assert_eq!(host.state().finished.len(), 1);
    assert!(host.pending_for(1));
    assert!(host.disconnect().is_err());
    for doc in 2..100 {
        assert!(host.choose(doc, 1, "Other.zip".into(), false).is_err());
    }
    assert_eq!(host.take_events().len(), super::MAX_EVENTS);
    let terminal = host.take_events();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].metadata["type"], "saveFailed");
    assert_eq!(terminal[0].metadata["requestId"], save);
    assert_eq!(terminal[0].metadata["error"], "encoder failed");
    assert!(!host.pending_for(1));
    assert!(!host.busy());
    host.failed(&save, "late duplicate");
    assert!(host.take_events().is_empty());
}

#[test]
fn retained_terminals_are_bounded_by_already_admitted_reads_and_saves() {
    let (host, connection) = connected();
    host.set_resources(&connection, vec![Resource { id: "one".into(), name: "One.zip".into(), updated_at: String::new() }]).unwrap();
    let mut saves = Vec::new();
    let mut reads = Vec::new();
    for doc in 1..=super::MAX_PENDING as u64 {
        let save = request(&host, doc);
        host.resolve_dialog(&save, "provider", "").unwrap();
        host.take_commands();
        saves.push(save);
        let read = host.request_open("one").unwrap();
        host.open(read.clone(), "One.zip".into(), vec![]).unwrap();
        host.take_commands();
        host.take_events();
        reads.push(read);
    }
    fill_event_queue(&host);
    for read in &reads {
        host.opened(read, None, "decode failed");
    }
    for save in &saves {
        host.failed(save, "encode failed");
    }
    assert_eq!(host.state().finished.len(), super::MAX_PENDING * 2);
    assert!(host.request_open("one").is_err());
    assert!(host.choose(999, 1, "New.zip".into(), false).is_err());
    assert_eq!(host.take_events().len(), super::MAX_EVENTS);
    let terminals = host.take_events();
    assert_eq!(terminals.len(), super::MAX_PENDING * 2);
    let expected: Vec<&str> = reads.iter().chain(saves.iter()).map(String::as_str).collect();
    let actual: Vec<&str> = terminals.iter().map(|event| event.metadata["requestId"].as_str().unwrap()).collect();
    assert_eq!(actual, expected, "retained open failures precede later save failures");
    for event in &terminals {
        assert_eq!(event.metadata["connectionId"], connection);
    }
    assert!(terminals.len() <= super::MAX_EVENTS);
    assert!(!host.busy());
    assert!(host.take_events().is_empty());
}
