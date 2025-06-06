//! Client dialog tests
//!
//! Tests for client-side dialog behavior and state management

use crate::dialog::{
    client_dialog::ClientInviteDialog,
    dialog::{DialogInner, DialogState, TerminatedReason},
    DialogId,
};
use crate::transaction::{endpoint::EndpointBuilder, key::TransactionRole};
use crate::transport::TransportLayer;
use rsip::{headers::*, Request, Response, StatusCode, Uri};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

async fn create_test_endpoint() -> crate::Result<crate::transaction::endpoint::Endpoint> {
    let token = CancellationToken::new();
    let tl = TransportLayer::new(token.child_token());
    let endpoint = EndpointBuilder::new()
        .with_user_agent("rsipstack-test")
        .with_transport_layer(tl)
        .build();
    Ok(endpoint)
}

fn create_invite_request(from_tag: &str, to_tag: &str, call_id: &str) -> Request {
    Request {
        method: rsip::Method::Invite,
        uri: Uri::try_from("sip:bob@example.com:5060").unwrap(),
        headers: vec![
            Via::new("SIP/2.0/UDP alice.example.com:5060;branch=z9hG4bKnashds").into(),
            CSeq::new("1 INVITE").into(),
            From::new(&format!("Alice <sip:alice@example.com>;tag={}", from_tag)).into(),
            To::new(&format!("Bob <sip:bob@example.com>;tag={}", to_tag)).into(),
            CallId::new(call_id).into(),
            Contact::new("<sip:alice@alice.example.com:5060>").into(),
            MaxForwards::new("70").into(),
        ]
        .into(),
        version: rsip::Version::V2,
        body: b"v=0\r\no=alice 2890844526 2890844527 IN IP4 host.atlanta.com\r\n".to_vec(),
    }
}

#[tokio::test]
async fn test_client_dialog_creation() -> crate::Result<()> {
    let endpoint = create_test_endpoint().await?;
    let (state_sender, _) = unbounded_channel();

    let dialog_id = DialogId {
        call_id: "test-call-id".to_string(),
        from_tag: "alice-tag".to_string(),
        to_tag: "bob-tag".to_string(),
    };

    let invite_req = create_invite_request("alice-tag", "", "test-call-id");

    let dialog_inner = DialogInner::new(
        TransactionRole::Client,
        dialog_id.clone(),
        invite_req,
        endpoint.inner.clone(),
        state_sender,
        None,
        Some(Uri::try_from("sip:alice@alice.example.com:5060").unwrap()),
    )?;

    let client_dialog = ClientInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Test initial state
    assert_eq!(client_dialog.id(), dialog_id);
    assert!(!client_dialog.inner.is_confirmed());

    Ok(())
}

#[tokio::test]
async fn test_client_dialog_sequence_handling() -> crate::Result<()> {
    let endpoint = create_test_endpoint().await?;
    let (state_sender, _) = unbounded_channel();

    let dialog_id = DialogId {
        call_id: "test-call-seq".to_string(),
        from_tag: "alice-tag".to_string(),
        to_tag: "bob-tag".to_string(),
    };

    let invite_req = create_invite_request("alice-tag", "bob-tag", "test-call-seq");

    let dialog_inner = DialogInner::new(
        TransactionRole::Client,
        dialog_id.clone(),
        invite_req,
        endpoint.inner.clone(),
        state_sender,
        None,
        Some(Uri::try_from("sip:alice@alice.example.com:5060").unwrap()),
    )?;

    let client_dialog = ClientInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Test initial sequence
    let initial_seq = client_dialog.inner.get_local_seq();
    assert_eq!(initial_seq, 1);

    // Test sequence increment
    let next_seq = client_dialog.inner.increment_local_seq();
    assert_eq!(next_seq, 2);

    Ok(())
}

#[tokio::test]
async fn test_client_dialog_state_transitions() -> crate::Result<()> {
    let endpoint = create_test_endpoint().await?;
    let (state_sender, _) = unbounded_channel();

    let dialog_id = DialogId {
        call_id: "test-call-flow".to_string(),
        from_tag: "alice-tag".to_string(),
        to_tag: "".to_string(),
    };

    let invite_req = create_invite_request("alice-tag", "", "test-call-flow");

    let dialog_inner = DialogInner::new(
        TransactionRole::Client,
        dialog_id.clone(),
        invite_req,
        endpoint.inner.clone(),
        state_sender,
        None,
        Some(Uri::try_from("sip:alice@alice.example.com:5060").unwrap()),
    )?;

    let client_dialog = ClientInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Test state transitions manually (simulating what happens during invite flow)

    // Initial state should be Calling
    let state = client_dialog.inner.state.lock().unwrap().clone();
    assert!(matches!(state, DialogState::Calling(_)));

    // Transition to Trying (after sending INVITE)
    client_dialog
        .inner
        .transition(DialogState::Trying(dialog_id.clone()))?;
    let state = client_dialog.inner.state.lock().unwrap().clone();
    assert!(matches!(state, DialogState::Trying(_)));

    // Transition to Early (after receiving 1xx)
    let ringing_resp = Response {
        status_code: StatusCode::Ringing,
        version: rsip::Version::V2,
        headers: vec![
            Via::new("SIP/2.0/UDP alice.example.com:5060;branch=z9hG4bKnashds").into(),
            CSeq::new("1 INVITE").into(),
            From::new("Alice <sip:alice@example.com>;tag=alice-tag").into(),
            To::new("Bob <sip:bob@example.com>;tag=bob-tag").into(),
            CallId::new("test-call-flow").into(),
            Contact::new("<sip:bob@bob.example.com:5060>").into(),
        ]
        .into(),
        body: vec![],
    };

    client_dialog
        .inner
        .transition(DialogState::Early(dialog_id.clone(), ringing_resp))?;
    let state = client_dialog.inner.state.lock().unwrap().clone();
    assert!(matches!(state, DialogState::Early(_, _)));

    // Transition to Confirmed (after receiving 200 OK and sending ACK)
    client_dialog
        .inner
        .transition(DialogState::Confirmed(dialog_id.clone()))?;
    let state = client_dialog.inner.state.lock().unwrap().clone();
    assert!(matches!(state, DialogState::Confirmed(_)));
    assert!(client_dialog.inner.is_confirmed());

    Ok(())
}

#[tokio::test]
async fn test_client_dialog_termination_scenarios() -> crate::Result<()> {
    let endpoint = create_test_endpoint().await?;
    let (state_sender, _) = unbounded_channel();

    // Test 1: Early termination (before confirmed)
    let dialog_id_1 = DialogId {
        call_id: "test-call-term-early".to_string(),
        from_tag: "alice-tag".to_string(),
        to_tag: "".to_string(),
    };

    let invite_req_1 = create_invite_request("alice-tag", "", "test-call-term-early");
    let dialog_inner_1 = DialogInner::new(
        TransactionRole::Client,
        dialog_id_1.clone(),
        invite_req_1,
        endpoint.inner.clone(),
        state_sender.clone(),
        None,
        Some(Uri::try_from("sip:alice@alice.example.com:5060").unwrap()),
    )?;

    let client_dialog_1 = ClientInviteDialog {
        inner: Arc::new(dialog_inner_1),
    };

    // Terminate early with error
    client_dialog_1.inner.transition(DialogState::Terminated(
        dialog_id_1.clone(),
        TerminatedReason::UasBusy,
    ))?;

    let state = client_dialog_1.inner.state.lock().unwrap().clone();
    assert!(matches!(
        state,
        DialogState::Terminated(_, TerminatedReason::UasBusy)
    ));

    // Test 2: Normal termination after confirmed
    let dialog_id_2 = DialogId {
        call_id: "test-call-term-normal".to_string(),
        from_tag: "alice-tag".to_string(),
        to_tag: "bob-tag".to_string(),
    };

    let invite_req_2 = create_invite_request("alice-tag", "bob-tag", "test-call-term-normal");
    let dialog_inner_2 = DialogInner::new(
        TransactionRole::Client,
        dialog_id_2.clone(),
        invite_req_2,
        endpoint.inner.clone(),
        state_sender,
        None,
        Some(Uri::try_from("sip:alice@alice.example.com:5060").unwrap()),
    )?;

    let client_dialog_2 = ClientInviteDialog {
        inner: Arc::new(dialog_inner_2),
    };

    // Confirm dialog first
    client_dialog_2
        .inner
        .transition(DialogState::Confirmed(dialog_id_2.clone()))?;
    assert!(client_dialog_2.inner.is_confirmed());

    // Then terminate normally
    client_dialog_2.inner.transition(DialogState::Terminated(
        dialog_id_2.clone(),
        TerminatedReason::UacBye,
    ))?;
    let state = client_dialog_2.inner.state.lock().unwrap().clone();
    assert!(matches!(
        state,
        DialogState::Terminated(_, TerminatedReason::UacBye)
    ));

    Ok(())
}
