//! The account routes (`account.v1`) of the HTTP transport against the public test server:
//! response validation, the read-only state and refused actions.

use penroam_sync::transport::{HttpTransport, TransportError};
use penroam_sync_protocol::account::{actions, read_only_reasons};
use penroam_sync_protocol::{BillingLinkRequest, ErrorCode};
use penroam_sync_testkit::{http, InMemoryServer};
use std::sync::Arc;

#[test]
fn account_state_and_refused_actions() {
    let server = Arc::new(InMemoryServer::new());
    let handle = http::spawn("127.0.0.1:0", server.clone()).unwrap();
    let (token, account) =
        HttpTransport::test_session(&handle.base_url, "acct-plan", None).unwrap();
    let t = HttpTransport::new(&handle.base_url, token);

    let s = t.account_state().unwrap();
    assert_eq!(s.account_id.as_str(), account);
    assert!(s.writable);
    assert!(s.offers.is_empty() && s.actions.is_empty());
    assert!(!s.allows(actions::SUBSCRIBE));

    server.set_quota(&s.account_id, Some(0));
    let s = t.account_state().unwrap();
    assert!(!s.writable);
    assert_eq!(
        s.read_only_reason.as_deref(),
        Some(read_only_reasons::OVER_QUOTA)
    );

    let e = t
        .billing_link(&BillingLinkRequest {
            action: actions::SUBSCRIBE.into(),
            offer_id: Some("anything".into()),
        })
        .unwrap_err();
    assert!(
        matches!(e, TransportError::Api(ref b) if b.code == ErrorCode::Forbidden),
        "{e:?}"
    );
    assert!(t.request_deletion().is_err());
    handle.shutdown();
}
