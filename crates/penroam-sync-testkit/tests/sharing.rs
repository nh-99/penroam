//! Shared notebooks in the development test server: the permission matrix for every sharing
//! route and the space sync routes (owner, editor, viewer, removed member, other account,
//! unknown account, forged device / space ids), the invitation lifecycle, atomicity of
//! concurrent accept / cancel / removal, quota grouping and persistence.
//!
//! Every refusal is checked with [`deny`], which also asserts that the server state is unchanged.

use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::sharing::{
    ChangeRoleRequest, CreateInvitationRequest, CreateSpaceRequest, InvitationState,
    MembershipState, Role, UpdateSpaceMetaRequest, INVITATION_TTL_MS,
};
use penroam_sync_protocol::{
    AccountId, AccountKeyDocument, AckRequest, AckResult, BlobId, CheckpointRecord, DeviceId,
    DeviceRegistrationRequest, EnvelopeHeader, ErrorBody, ErrorCode, InvitationId,
    KeyLogAppendRequest, Limits, PullOpsRequest,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{Caller, InMemoryServer, RequestError};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}

fn code<T: std::fmt::Debug>(r: Result<T, RequestError>) -> ErrorCode {
    body(r).code
}

fn body<T: std::fmt::Debug>(r: Result<T, RequestError>) -> ErrorBody {
    match r {
        Err(RequestError::Api(b)) => b,
        other => panic!("expected an API error, got {other:?}"),
    }
}

/// Runs a request that must be refused with `expected` and must not change any server state.
#[track_caller]
fn deny<T: std::fmt::Debug>(
    s: &InMemoryServer,
    expected: ErrorCode,
    f: impl FnOnce() -> Result<T, RequestError>,
) {
    let before = s.state_fingerprint();
    assert_eq!(code(f()), expected);
    assert_eq!(
        s.state_fingerprint(),
        before,
        "a refused request changed server state"
    );
}

fn reg(id: &str) -> DeviceRegistrationRequest {
    DeviceRegistrationRequest {
        device_id: Some(dev(id)),
        signing_public_key: base64url::encode(&[7; 32]),
        encryption_public_key: base64url::encode(&[8; 32]),
        label: Some("Device".into()),
    }
}

fn meta(label: &str) -> String {
    base64url::encode(&fake_ciphertext(label, 60))
}

fn op(device: &str, n: u64) -> (EnvelopeHeader, Vec<u8>) {
    let ct = fake_ciphertext(&format!("{device}-{n}"), 40);
    (
        header_for(&format!("op-{device}-{n}"), device, n, &[], &ct),
        ct,
    )
}

fn batch(items: &[(EnvelopeHeader, Vec<u8>)]) -> Vec<u8> {
    encode_json_batch(items, &Limits::DEFAULT).unwrap()
}

/// A key-log entry as clients write it (protocol §14); only the fields the server reads matter.
fn keylog_entry(index: u64, entry: serde_json::Value) -> KeyLogAppendRequest {
    let body = serde_json::json!({"v": 1, "index": index, "prev": "", "entry": entry});
    let signed = serde_json::json!({
        "body": base64url::encode(&serde_json::to_vec(&body).unwrap()),
        "signer": "account",
        "sig": base64url::encode(&[0xA5; 64]),
    });
    KeyLogAppendRequest {
        index,
        data: base64url::encode(&serde_json::to_vec(&signed).unwrap()),
    }
}

fn add_device(index: u64, device: &str) -> KeyLogAppendRequest {
    keylog_entry(
        index,
        serde_json::json!({"kind": "addDevice", "device": {"deviceId": device,
            "signPk": "AA", "dhPk": "AA", "label": "Device"}, "grants": []}),
    )
}

fn invite(email: &str, role: Role, key: &str) -> CreateInvitationRequest {
    CreateInvitationRequest {
        email: email.into(),
        role,
        idempotency_key: key.into(),
    }
}

const T0: u64 = 1_800_000_000_000;

/// A space owned by `owner` with an active editor and viewer, a removed member and an outsider,
/// each member with one device registered in the space, a stored key and a genesis entry.
struct World {
    s: Arc<InMemoryServer>,
    space: AccountId,
    owner: AccountId,
    editor: AccountId,
    viewer: AccountId,
    revoked: AccountId,
    outsider: AccountId,
}

impl World {
    fn new() -> Self {
        Self::with(InMemoryServer::new())
    }

    fn with(server: InMemoryServer) -> Self {
        let s = Arc::new(server);
        s.set_clock_ms(Some(T0));
        let [owner, editor, viewer, revoked, outsider] = [
            "acct-owner",
            "acct-editor",
            "acct-viewer",
            "acct-revoked",
            "acct-outsider",
        ]
        .map(acct);
        for (a, e) in [
            (&owner, "ada@example.org"),
            (&editor, "bob@example.org"),
            (&viewer, "carol@example.org"),
            (&revoked, "dan@example.org"),
            (&outsider, "eve@example.org"),
        ] {
            s.set_verified_email(a, Some(e)).unwrap();
        }
        let rec = s
            .shared_create(
                &owner,
                &CreateSpaceRequest {
                    idempotency_key: "create-1".into(),
                    sealed_meta: meta("title"),
                },
            )
            .unwrap();
        assert!(rec.owner && rec.role == Role::Owner);
        let space = rec.space_id;
        s.shared_key_put(
            &owner,
            &space,
            &AccountKeyDocument {
                key: base64url::encode(&[9; 32]),
            },
        )
        .unwrap();
        for (who, email, role) in [
            (&editor, "bob@example.org", Role::Editor),
            (&viewer, "carol@example.org", Role::Viewer),
            (&revoked, "dan@example.org", Role::Editor),
        ] {
            let inv = s
                .shared_invite(
                    &owner,
                    &space,
                    &invite(email, role, email.split('@').next().unwrap()),
                )
                .unwrap();
            let joined = s.invitation_accept(who, &inv.invitation_id).unwrap();
            assert_eq!(joined.role, role);
        }
        for (who, d) in [
            (&owner, "dev-owner"),
            (&editor, "dev-editor"),
            (&viewer, "dev-viewer"),
            (&revoked, "dev-revoked"),
        ] {
            s.register_device_keys(Caller::member(&space, who), &reg(d))
                .unwrap();
        }
        s.keylog_append(
            Caller::member(&space, &owner),
            &keylog_entry(0, serde_json::json!({"kind": "genesis"})),
        )
        .unwrap();
        s.shared_remove_member(&owner, &space, &revoked).unwrap();
        World {
            s,
            space,
            owner,
            editor,
            viewer,
            revoked,
            outsider,
        }
    }

    fn m<'a>(&'a self, who: &'a AccountId) -> Caller<'a> {
        Caller::member(&self.space, who)
    }
}

// ------------------------------------------------------------ sharing routes --

#[test]
fn sharing_routes_permission_matrix() {
    let w = World::new();
    let s = &*w.s;
    let sp = &w.space;
    let none: AccountId = acct("acct-unknown");

    // Unknown accounts are unauthenticated everywhere.
    deny(s, ErrorCode::Unauthenticated, || s.shared_list(&none));
    deny(s, ErrorCode::Unauthenticated, || {
        s.shared_members(&none, sp)
    });
    deny(s, ErrorCode::Unauthenticated, || {
        s.invitations_incoming(&none)
    });

    // Sealed meta (notebook id and title): owner only, since an editor could repoint the id.
    s.shared_set_meta(
        &w.owner,
        sp,
        &UpdateSpaceMetaRequest {
            sealed_meta: meta("t2"),
        },
    )
    .unwrap();
    let rename = UpdateSpaceMetaRequest {
        sealed_meta: meta("t3"),
    };
    for who in [&w.editor, &w.viewer] {
        deny(s, ErrorCode::Forbidden, || {
            s.shared_set_meta(who, sp, &rename)
        });
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_set_meta(&w.revoked, sp, &rename)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_set_meta(&w.outsider, sp, &rename)
    });
    let bad = UpdateSpaceMetaRequest {
        sealed_meta: "not base64!".into(),
    };
    deny(s, ErrorCode::ValidationFailed, || {
        s.shared_set_meta(&w.owner, sp, &bad)
    });

    // Sessions and the key: every active member.
    for who in [&w.owner, &w.editor, &w.viewer] {
        assert!(s.shared_session_check(who, sp).is_ok());
        assert_eq!(
            s.shared_key_get(who, sp).unwrap().key,
            base64url::encode(&[9; 32])
        );
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_session_check(&w.revoked, sp)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_key_get(&w.revoked, sp)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_session_check(&w.outsider, sp)
    });
    deny(s, ErrorCode::NotFound, || s.shared_key_get(&w.outsider, sp));
    let key = AccountKeyDocument {
        key: base64url::encode(&[1; 32]),
    };
    deny(s, ErrorCode::Conflict, || {
        s.shared_key_put(&w.owner, sp, &key)
    });
    for who in [&w.editor, &w.viewer] {
        deny(s, ErrorCode::Forbidden, || s.shared_key_put(who, sp, &key));
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_key_put(&w.revoked, sp, &key)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_key_put(&w.outsider, sp, &key)
    });

    // Members: every active member; invitations only for the owner; emails only to members.
    let owner_view = s.shared_members(&w.owner, sp).unwrap();
    assert_eq!(owner_view.members.len(), 4);
    assert_eq!(owner_view.invitations.len(), 3);
    assert_eq!(owner_view.members[0].role, Role::Owner);
    assert!(owner_view.members[0].is_you);
    let revoked = owner_view
        .members
        .iter()
        .find(|m| m.account_id == w.revoked)
        .unwrap();
    assert_eq!(revoked.state, MembershipState::Revoked);
    for who in [&w.editor, &w.viewer] {
        let v = s.shared_members(who, sp).unwrap();
        assert!(v.invitations.is_empty());
        assert_eq!(v.members.iter().filter(|m| m.is_you).count(), 1);
        assert!(v.members.iter().any(|m| m.display == "ada@example.org"));
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_members(&w.revoked, sp)
    });
    deny(s, ErrorCode::NotFound, || s.shared_members(&w.outsider, sp));

    // Inviting, role changes and removal: owner only.
    let inv = invite("frank@example.org", Role::Viewer, "frank");
    for who in [&w.editor, &w.viewer] {
        deny(s, ErrorCode::Forbidden, || s.shared_invite(who, sp, &inv));
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_invite(&w.revoked, sp, &inv)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_invite(&w.outsider, sp, &inv)
    });
    deny(s, ErrorCode::InvalidRole, || {
        s.shared_invite(
            &w.owner,
            sp,
            &invite("frank@example.org", Role::Owner, "f2"),
        )
    });
    deny(s, ErrorCode::ValidationFailed, || {
        s.shared_invite(&w.owner, sp, &invite("not an email", Role::Viewer, "f3"))
    });
    deny(s, ErrorCode::Conflict, || {
        s.shared_invite(&w.owner, sp, &invite("BOB@example.org", Role::Viewer, "f4"))
    });
    let invitation = s.shared_invite(&w.owner, sp, &inv).unwrap();

    let viewer_version = owner_view
        .members
        .iter()
        .find(|m| m.account_id == w.viewer)
        .unwrap()
        .version;
    let promote = ChangeRoleRequest {
        role: Role::Editor,
        version: viewer_version,
    };
    for who in [&w.editor, &w.viewer] {
        deny(s, ErrorCode::Forbidden, || {
            s.shared_change_role(who, sp, &w.viewer, &promote)
        });
        deny(s, ErrorCode::Forbidden, || {
            s.shared_remove_member(who, sp, &w.owner)
        });
    }
    deny(s, ErrorCode::Forbidden, || {
        s.shared_remove_member(&w.editor, sp, &w.viewer)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_change_role(&w.revoked, sp, &w.viewer, &promote)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.shared_remove_member(&w.revoked, sp, &w.viewer)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_change_role(&w.outsider, sp, &w.viewer, &promote)
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_remove_member(&w.outsider, sp, &w.viewer)
    });
    deny(s, ErrorCode::InvalidRole, || {
        s.shared_change_role(
            &w.owner,
            sp,
            &w.viewer,
            &ChangeRoleRequest {
                role: Role::Owner,
                version: viewer_version,
            },
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.shared_change_role(
            &w.owner,
            sp,
            &w.owner,
            &ChangeRoleRequest {
                role: Role::Viewer,
                version: 1,
            },
        )
    });
    deny(s, ErrorCode::Conflict, || {
        s.shared_change_role(
            &w.owner,
            sp,
            &w.viewer,
            &ChangeRoleRequest {
                role: Role::Editor,
                version: viewer_version + 7,
            },
        )
    });
    deny(s, ErrorCode::Conflict, || {
        s.shared_change_role(
            &w.owner,
            sp,
            &w.revoked,
            &ChangeRoleRequest {
                role: Role::Viewer,
                version: 2,
            },
        )
    });
    deny(s, ErrorCode::NotFound, || {
        s.shared_change_role(&w.owner, sp, &w.outsider, &promote)
    });
    deny(s, ErrorCode::Forbidden, || {
        s.shared_remove_member(&w.owner, sp, &w.owner)
    });
    let changed = s
        .shared_change_role(&w.owner, sp, &w.viewer, &promote)
        .unwrap();
    assert_eq!(changed.role, Role::Editor);
    assert_eq!(changed.version, viewer_version + 1);
    // the same request again is stale now
    deny(s, ErrorCode::Conflict, || {
        s.shared_change_role(&w.owner, sp, &w.viewer, &promote)
    });

    // Cancelling: owner only; everyone outside the space gets `not_found`.
    let inv_id = &invitation.invitation_id;
    deny(s, ErrorCode::NotFound, || {
        s.invitation_cancel(&w.editor, inv_id)
    });
    deny(s, ErrorCode::NotFound, || {
        s.invitation_cancel(&w.revoked, inv_id)
    });
    deny(s, ErrorCode::NotFound, || {
        s.invitation_cancel(&w.outsider, inv_id)
    });
    // Accepting or declining someone else's invitation: `not_found`.
    for who in [&w.owner, &w.editor, &w.outsider] {
        deny(s, ErrorCode::NotFound, || s.invitation_accept(who, inv_id));
        deny(s, ErrorCode::NotFound, || s.invitation_decline(who, inv_id));
    }
    s.invitation_cancel(&w.owner, inv_id).unwrap();
}

#[test]
fn space_and_vault_sessions_cannot_use_sharing_routes() {
    let w = World::new();
    let s = &*w.s;
    deny(s, ErrorCode::Forbidden, || s.shared_list(&w.space));
    deny(s, ErrorCode::Forbidden, || s.invitations_incoming(&w.space));
    deny(s, ErrorCode::Forbidden, || {
        s.shared_create(
            &w.space,
            &CreateSpaceRequest {
                idempotency_key: "nested".into(),
                sealed_meta: meta("n"),
            },
        )
    });
    let vault = s
        .vault_create(
            &w.owner,
            &penroam_sync_protocol::CreateVaultRequest {
                lock: penroam_sync_protocol::VaultLock {
                    kdf: "argon2id".into(),
                    memory_kib: 65536,
                    iterations: 3,
                    parallelism: 1,
                    salt: "c2FsdA".into(),
                    wrapped_root: "AAAA".into(),
                },
                name: "AAAA".into(),
            },
        )
        .unwrap()
        .vault_id;
    deny(s, ErrorCode::Forbidden, || s.shared_list(&vault));
    // The account-key routes are not a way into a space's key, for any member.
    for who in [&w.owner, &w.editor] {
        deny(s, ErrorCode::Forbidden, || s.account_key_get(&w.space));
        deny(s, ErrorCode::Forbidden, || {
            s.account_key_put(
                &w.space,
                &AccountKeyDocument {
                    key: base64url::encode(&[3; 32]),
                },
            )
        });
        let _ = who;
    }
}

#[test]
fn non_members_cannot_tell_real_spaces_from_unknown_ids() {
    let w = World::new();
    let s = &*w.s;
    let fake = acct("s-00000000000000000000000000000000");
    let real = body(s.shared_members(&w.outsider, &w.space));
    let unknown = body(s.shared_members(&w.outsider, &fake));
    assert_eq!((real.code, real.message), (unknown.code, unknown.message));
    let real = body(s.push(Caller::member(&w.space, &w.outsider), &dev("dev-x"), &[]));
    let unknown_member = body(s.shared_key_get(&w.outsider, &fake));
    assert_eq!(real.code, ErrorCode::NotFound);
    assert_eq!(unknown_member.code, ErrorCode::NotFound);
}

#[test]
fn creating_a_space_is_idempotent_and_lists_show_revoked_memberships() {
    let w = World::new();
    let s = &*w.s;
    let again = s
        .shared_create(
            &w.owner,
            &CreateSpaceRequest {
                idempotency_key: "create-1".into(),
                sealed_meta: meta("ignored"),
            },
        )
        .unwrap();
    assert_eq!(again.space_id, w.space);
    assert_eq!(s.shared_list(&w.owner).unwrap().spaces.len(), 1);
    let revoked = s.shared_list(&w.revoked).unwrap().spaces;
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].state, MembershipState::Revoked);
    assert!(s.shared_list(&w.outsider).unwrap().spaces.is_empty());
    let viewer = s.shared_list(&w.viewer).unwrap().spaces;
    assert_eq!((viewer[0].role, viewer[0].owner), (Role::Viewer, false));
    deny(s, ErrorCode::ValidationFailed, || {
        s.shared_create(
            &w.owner,
            &CreateSpaceRequest {
                idempotency_key: "bad key!".into(),
                sealed_meta: meta("x"),
            },
        )
    });
    deny(s, ErrorCode::ValidationFailed, || {
        s.shared_create(
            &w.owner,
            &CreateSpaceRequest {
                idempotency_key: "k".into(),
                sealed_meta: "A".repeat(4100),
            },
        )
    });
}

// ---------------------------------------------------------------- sync routes --

#[test]
fn space_sync_routes_permission_matrix() {
    let w = World::new();
    let s = &*w.s;
    let sp = &w.space;

    // Push: owner and editor write; viewer `forbidden`; removed `access_revoked`; others
    // `not_found`.
    for (who, d) in [(&w.owner, "dev-owner"), (&w.editor, "dev-editor")] {
        let r = s.push(w.m(who), &dev(d), &batch(&[op(d, 1)])).unwrap();
        assert!(matches!(r.acks[0].result, AckResult::Accepted { .. }));
    }
    let viewer_batch = batch(&[op("dev-viewer", 1)]);
    deny(s, ErrorCode::Forbidden, || {
        s.push(w.m(&w.viewer), &dev("dev-viewer"), &viewer_batch)
    });
    // a device session alone resolves the member from the device
    deny(s, ErrorCode::Forbidden, || {
        s.push(sp, &dev("dev-viewer"), &viewer_batch)
    });
    let revoked_batch = batch(&[op("dev-revoked", 1)]);
    deny(s, ErrorCode::AccessRevoked, || {
        s.push(w.m(&w.revoked), &dev("dev-revoked"), &revoked_batch)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.push(sp, &dev("dev-revoked"), &revoked_batch)
    });
    deny(s, ErrorCode::NotFound, || {
        s.push(
            w.m(&w.outsider),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", 2)]),
        )
    });

    // Forged device ids: someone else's device in the session, or in the envelope.
    deny(s, ErrorCode::Forbidden, || {
        s.push(
            w.m(&w.editor),
            &dev("dev-owner"),
            &batch(&[op("dev-owner", 2)]),
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.push(
            w.m(&w.viewer),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", 2)]),
        )
    });
    let before = s.state_fingerprint();
    let forged = s
        .push(
            w.m(&w.editor),
            &dev("dev-editor"),
            &batch(&[op("dev-owner", 2)]),
        )
        .unwrap();
    assert!(matches!(
        &forged.acks[0].result,
        AckResult::Rejected { error } if error.code == ErrorCode::Forbidden
    ));
    assert_eq!(s.state_fingerprint(), before);
    // A member's session on a space they don't belong to.
    let other = s
        .shared_create(
            &w.outsider,
            &CreateSpaceRequest {
                idempotency_key: "o".into(),
                sealed_meta: meta("o"),
            },
        )
        .unwrap()
        .space_id;
    deny(s, ErrorCode::NotFound, || {
        s.push(
            Caller::member(&other, &w.editor),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", 2)]),
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.push(
            Caller::member(&w.owner, &w.editor),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", 2)]),
        )
    });

    // Reads: every active member, never the removed one or outsiders.
    let req = PullOpsRequest {
        cursor: None,
        limit: 10,
    };
    for (who, d) in [
        (&w.owner, "dev-owner"),
        (&w.editor, "dev-editor"),
        (&w.viewer, "dev-viewer"),
    ] {
        let page = s.pull(w.m(who), &dev(d), &req).unwrap();
        assert_eq!(page.response.items.len(), 2);
        s.ack(
            w.m(who),
            &dev(d),
            &AckRequest {
                applied_cursor: page.response.next_cursor.clone(),
            },
        )
        .unwrap();
        assert_eq!(s.keylog_get(w.m(who), 0).unwrap().entries.len(), 1);
        assert_eq!(s.list_devices(w.m(who)).unwrap().devices.len(), 4);
        assert_eq!(s.usage(w.m(who)).unwrap().op_count, 2);
        assert_eq!(
            code(s.checkpoint_latest(w.m(who), &dev(d))),
            ErrorCode::NotFound
        );
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.pull(w.m(&w.revoked), &dev("dev-revoked"), &req)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.keylog_get(w.m(&w.revoked), 0)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.list_devices(w.m(&w.revoked))
    });
    deny(s, ErrorCode::AccessRevoked, || s.usage(w.m(&w.revoked)));
    deny(s, ErrorCode::AccessRevoked, || {
        s.account_state(w.m(&w.revoked))
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.checkpoint_latest(w.m(&w.revoked), &dev("dev-revoked"))
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.blob_get(
            w.m(&w.revoked),
            &dev("dev-revoked"),
            &"b-1".parse().unwrap(),
        )
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.register_device_keys(w.m(&w.revoked), &reg("dev-revoked-2"))
    });
    deny(s, ErrorCode::NotFound, || s.keylog_get(w.m(&w.outsider), 0));
    deny(s, ErrorCode::NotFound, || s.list_devices(w.m(&w.outsider)));
    deny(s, ErrorCode::NotFound, || {
        s.register_device_keys(w.m(&w.outsider), &reg("dev-x"))
    });
    // A space request without any member identity.
    deny(s, ErrorCode::Forbidden, || s.keylog_get(sp, 0));

    // Blobs and checkpoints: writers only.
    let bytes = fake_ciphertext("blob", 64);
    let sha = sha256_hex(&bytes);
    let blob: BlobId = "b-shared".parse().unwrap();
    deny(s, ErrorCode::Forbidden, || {
        s.blob_put(w.m(&w.viewer), &dev("dev-viewer"), &blob, &sha, &bytes)
    });
    deny(s, ErrorCode::AccessRevoked, || {
        s.blob_put(w.m(&w.revoked), &dev("dev-revoked"), &blob, &sha, &bytes)
    });
    s.blob_put(w.m(&w.editor), &dev("dev-editor"), &blob, &sha, &bytes)
        .unwrap();
    assert_eq!(
        s.blob_get(w.m(&w.viewer), &dev("dev-viewer"), &blob)
            .unwrap(),
        bytes
    );
    let page = s.pull(w.m(&w.editor), &dev("dev-editor"), &req).unwrap();
    let rec = |by: &str| CheckpointRecord {
        covers_cursor: page.response.next_cursor.clone(),
        covers_server_seq: 2,
        blob_id: blob.clone(),
        blob_sha256: sha.clone(),
        blob_length: bytes.len() as u64,
        key_epoch: 1,
        created_by: dev(by),
        signature: base64url::encode(&[0xA5; 64]),
    };
    // Checkpoints: the owner only. An editor could otherwise publish one that leaves out other
    // members' operations.
    for (who, d) in [(&w.editor, "dev-editor"), (&w.viewer, "dev-viewer")] {
        deny(s, ErrorCode::Forbidden, || {
            s.checkpoint_put(w.m(who), &dev(d), &rec(d))
        });
        // a device session alone resolves to the same member
        deny(s, ErrorCode::Forbidden, || {
            s.checkpoint_put(sp, &dev(d), &rec(d))
        });
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.checkpoint_put(w.m(&w.revoked), &dev("dev-revoked"), &rec("dev-revoked"))
    });
    deny(s, ErrorCode::NotFound, || {
        s.checkpoint_put(w.m(&w.outsider), &dev("dev-owner"), &rec("dev-owner"))
    });
    s.checkpoint_put(w.m(&w.owner), &dev("dev-owner"), &rec("dev-owner"))
        .unwrap();
    for (who, d) in [(&w.editor, "dev-editor"), (&w.viewer, "dev-viewer")] {
        assert_eq!(
            s.checkpoint_latest(w.m(who), &dev(d)).unwrap().created_by,
            dev("dev-owner")
        );
    }
}

#[test]
fn key_log_appends_by_members_are_limited_to_their_own_devices() {
    let w = World::new();
    let s = &*w.s;
    // Non-owners may add, rename and remove only devices they registered in the space.
    for (who, d) in [(&w.editor, "dev-editor"), (&w.viewer, "dev-viewer")] {
        let n = s.keylog_get(w.m(who), 0).unwrap().next_index;
        s.keylog_append(w.m(who), &add_device(n, d)).unwrap();
        let rename = keylog_entry(
            n + 1,
            serde_json::json!({"kind": "renameDevice", "deviceId": d, "label": "Tablet"}),
        );
        s.keylog_append(w.m(who), &rename).unwrap();
    }
    let n = s.keylog_get(w.m(&w.owner), 0).unwrap().next_index;
    for (who, mine, theirs) in [
        (&w.editor, "dev-editor", "dev-owner"),
        (&w.viewer, "dev-viewer", "dev-editor"),
    ] {
        deny(s, ErrorCode::Forbidden, || {
            s.keylog_append(w.m(who), &add_device(n, theirs))
        });
        deny(s, ErrorCode::Forbidden, || {
            s.keylog_append(
                w.m(who),
                &keylog_entry(
                    n,
                    serde_json::json!({"kind": "removeDevice", "deviceId": theirs,
                        "newEpoch": 2, "grants": []}),
                ),
            )
        });
        // A device id nobody registered, other kinds and garbage are refused too.
        deny(s, ErrorCode::Forbidden, || {
            s.keylog_append(w.m(who), &add_device(n, "dev-nobody"))
        });
        deny(s, ErrorCode::Forbidden, || {
            s.keylog_append(
                w.m(who),
                &keylog_entry(n, serde_json::json!({"kind": "genesis"})),
            )
        });
        deny(s, ErrorCode::Forbidden, || {
            s.keylog_append(
                w.m(who),
                &KeyLogAppendRequest {
                    index: n,
                    data: base64url::encode(b"{not json"),
                },
            )
        });
        let _ = mine;
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.keylog_append(w.m(&w.revoked), &add_device(n, "dev-revoked"))
    });
    deny(s, ErrorCode::NotFound, || {
        s.keylog_append(w.m(&w.outsider), &add_device(n, "dev-owner"))
    });
    // The owner may remove another member's device, but must grant the new epoch to every
    // remaining device and recovery like anyone else.
    deny(s, ErrorCode::ValidationFailed, || {
        s.keylog_append(
            w.m(&w.owner),
            &remove_device(n, "dev-revoked", &["recovery"]),
        )
    });
    s.keylog_append(
        w.m(&w.owner),
        &remove_device(n, "dev-revoked", &["dev-editor", "dev-viewer", "recovery"]),
    )
    .unwrap();
}

fn remove_device(index: u64, device: &str, grants_to: &[&str]) -> KeyLogAppendRequest {
    let grants: Vec<_> = grants_to
        .iter()
        .map(|to| serde_json::json!({"to": to, "epoch": 2, "wrapped": "AAAA"}))
        .collect();
    keylog_entry(
        index,
        serde_json::json!({"kind": "removeDevice", "deviceId": device, "newEpoch": 2,
            "grants": grants, "lastSeq": 0}),
    )
}

#[test]
fn a_device_removal_must_grant_the_new_epoch_to_everyone_remaining() {
    let w = World::new();
    let s = &*w.s;
    for (who, d) in [
        (&w.owner, "dev-owner"),
        (&w.editor, "dev-editor"),
        (&w.viewer, "dev-viewer"),
    ] {
        let n = s.keylog_get(w.m(who), 0).unwrap().next_index;
        s.keylog_append(w.m(who), &add_device(n, d)).unwrap();
    }
    let n = s.keylog_get(w.m(&w.viewer), 0).unwrap().next_index;
    // A viewer removing their own device with a grant to recovery only would start an epoch
    // nobody else can read.
    deny(s, ErrorCode::ValidationFailed, || {
        s.keylog_append(
            w.m(&w.viewer),
            &remove_device(n, "dev-viewer", &["recovery"]),
        )
    });
    // Missing recovery, a grant for the wrong epoch, or no newEpoch at all: the same.
    deny(s, ErrorCode::ValidationFailed, || {
        s.keylog_append(
            w.m(&w.viewer),
            &remove_device(n, "dev-viewer", &["dev-owner", "dev-editor"]),
        )
    });
    let wrong_epoch = keylog_entry(
        n,
        serde_json::json!({"kind": "removeDevice", "deviceId": "dev-viewer", "newEpoch": 3,
            "grants": [{"to": "dev-owner", "epoch": 2, "wrapped": "AAAA"},
                       {"to": "dev-editor", "epoch": 2, "wrapped": "AAAA"},
                       {"to": "recovery", "epoch": 2, "wrapped": "AAAA"}]}),
    );
    deny(s, ErrorCode::ValidationFailed, || {
        s.keylog_append(w.m(&w.viewer), &wrong_epoch)
    });
    let no_epoch = keylog_entry(
        n,
        serde_json::json!({"kind": "removeDevice", "deviceId": "dev-viewer", "grants": []}),
    );
    deny(s, ErrorCode::ValidationFailed, || {
        s.keylog_append(w.m(&w.viewer), &no_epoch)
    });
    // Ownership is still checked first: someone else's device is `forbidden`.
    deny(s, ErrorCode::Forbidden, || {
        s.keylog_append(
            w.m(&w.viewer),
            &remove_device(n, "dev-editor", &["dev-owner", "dev-viewer", "recovery"]),
        )
    });
    // A complete removal is accepted, and the removed device no longer needs a grant later.
    s.keylog_append(
        w.m(&w.viewer),
        &remove_device(n, "dev-viewer", &["dev-owner", "dev-editor", "recovery"]),
    )
    .unwrap();
    let n = n + 1;
    let later = keylog_entry(
        n,
        serde_json::json!({"kind": "removeDevice", "deviceId": "dev-editor", "newEpoch": 3,
            "grants": [{"to": "dev-owner", "epoch": 3, "wrapped": "AAAA"},
                       {"to": "recovery", "epoch": 3, "wrapped": "AAAA"}]}),
    );
    s.keylog_append(w.m(&w.editor), &later).unwrap();
}

#[test]
fn invitation_routes_answer_unknown_and_forbidden_ids_identically() {
    let w = World::new();
    let s = &*w.s;
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Viewer, "probe"),
        )
        .unwrap()
        .invitation_id;
    let unknown: InvitationId = "inv-00000000000000000000000000000000".parse().unwrap();
    let answer = |r: Result<(), RequestError>| {
        let b = body(r);
        (b.code, b.message)
    };
    for who in [&w.editor, &w.viewer, &w.revoked, &w.owner] {
        let accept = |id: &InvitationId| {
            let b = body(s.invitation_accept(who, id));
            (b.code, b.message)
        };
        assert_eq!(accept(&inv), accept(&unknown));
        assert_eq!(
            answer(s.invitation_decline(who, &inv)),
            answer(s.invitation_decline(who, &unknown))
        );
    }
    for who in [&w.editor, &w.viewer, &w.revoked, &w.outsider] {
        let before = s.state_fingerprint();
        assert_eq!(
            answer(s.invitation_cancel(who, &inv)),
            answer(s.invitation_cancel(who, &unknown))
        );
        assert_eq!(s.state_fingerprint(), before);
    }
    assert_eq!(
        answer(s.invitation_cancel(&w.owner, &unknown)).0,
        ErrorCode::NotFound
    );
}

#[test]
fn accepting_needs_a_recent_email_verification_when_the_server_asks() {
    let w = World::new();
    let s = &*w.s;
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Viewer, "fresh"),
        )
        .unwrap()
        .invitation_id;
    const HOUR: u64 = 3_600_000;
    s.set_reauth_window_ms(Some(HOUR));
    s.set_email_verified_at_ms(&w.outsider, T0 - HOUR - 1);
    deny(s, ErrorCode::ReauthRequired, || {
        s.invitation_accept(&w.outsider, &inv)
    });
    deny(s, ErrorCode::ReauthRequired, || {
        s.invitation_decline(&w.outsider, &inv)
    });
    // Listing never fails for this: it answers an empty list flagged `reauth_required`, and
    // changes nothing.
    let before = s.state_fingerprint();
    let stale = s.invitations_incoming(&w.outsider).unwrap();
    assert!(stale.invitations.is_empty() && stale.reauth_required);
    assert_eq!(s.state_fingerprint(), before);
    // Without any verified email there is nothing to list and no flag.
    let anon = acct("acct-anon");
    s.create_account(&anon);
    let none = s.invitations_incoming(&anon).unwrap();
    assert!(none.invitations.is_empty() && !none.reauth_required);
    // Someone the invitation isn't for still gets plain `not_found`.
    s.set_email_verified_at_ms(&w.editor, T0 - HOUR - 1);
    deny(s, ErrorCode::NotFound, || {
        s.invitation_accept(&w.editor, &inv)
    });
    // Signing in again (a fresh verification) lets the invitee list and accept.
    s.set_verified_email(&w.outsider, Some("eve@example.org"))
        .unwrap();
    assert!(!s.invitations_incoming(&w.outsider).unwrap().reauth_required);
    assert_eq!(
        s.invitations_incoming(&w.outsider)
            .unwrap()
            .invitations
            .len(),
        1
    );
    s.invitation_accept(&w.outsider, &inv).unwrap();
    // A verification exactly at the window's edge still counts.
    let inv2 = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("frank@example.org", Role::Viewer, "edge"),
        )
        .unwrap()
        .invitation_id;
    let frank = acct("acct-frank");
    s.set_verified_email(&frank, Some("frank@example.org"))
        .unwrap();
    s.advance_clock_ms(HOUR);
    s.invitation_accept(&frank, &inv2).unwrap();
}

#[test]
fn device_registration_and_removal_respect_members() {
    let w = World::new();
    let s = &*w.s;
    assert_eq!(
        s.device_registered_by(&w.space, &dev("dev-editor")),
        Some(w.editor.clone())
    );
    // A device id belongs to the member who registered it first.
    deny(s, ErrorCode::Forbidden, || {
        s.register_device_keys(w.m(&w.viewer), &reg("dev-editor"))
    });
    // Re-registering your own device is idempotent.
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor"))
        .unwrap();
    // Only the owner may remove another member's device.
    deny(s, ErrorCode::Forbidden, || {
        s.revoke_device_as(w.m(&w.editor), &dev("dev-editor"), &dev("dev-viewer"))
    });
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor-2"))
        .unwrap();
    s.revoke_device_as(w.m(&w.editor), &dev("dev-editor"), &dev("dev-editor-2"))
        .unwrap();
    s.revoke_device_as(w.m(&w.owner), &dev("dev-owner"), &dev("dev-viewer"))
        .unwrap();
    assert!(s.device_revoked(&w.space, &dev("dev-viewer")));
}

// --------------------------------------------------------- invitation lifecycle --

#[test]
fn invitation_answers_do_not_reveal_whether_an_account_exists() {
    let w = World::new();
    let s = &*w.s;
    let to_account = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Viewer, "e"),
        )
        .unwrap();
    let to_nobody = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("nobody@example.org", Role::Viewer, "n"),
        )
        .unwrap();
    assert_eq!(to_account.state, to_nobody.state);
    assert_eq!(to_account.role, to_nobody.role);
    assert_eq!(to_account.expires_at_ms, to_nobody.expires_at_ms);
    assert_eq!(to_account.version, to_nobody.version);
    let shape = |r: &penroam_sync_protocol::InvitationRecord| {
        let v = serde_json::to_value(r).unwrap();
        v.as_object().unwrap().keys().cloned().collect::<Vec<_>>()
    };
    assert_eq!(shape(&to_account), shape(&to_nobody));
    // Retrying with the same key returns the same invitation; reusing it differently conflicts.
    let retry = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite(" EVE@example.org", Role::Viewer, "e"),
        )
        .unwrap();
    assert_eq!(retry, to_account);
    deny(s, ErrorCode::Conflict, || {
        s.shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "e"),
        )
    });
    deny(s, ErrorCode::Conflict, || {
        s.shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "e2"),
        )
    });
    // The invitee sees it, without any notebook content.
    let incoming = s.invitations_incoming(&w.outsider).unwrap().invitations;
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].invited_by, "ada@example.org");
    assert_eq!(incoming[0].space_id, w.space);
    // An account without a verified email sees nothing.
    let anon = acct("acct-anon");
    s.create_account(&anon);
    assert!(s
        .invitations_incoming(&anon)
        .unwrap()
        .invitations
        .is_empty());
    deny(s, ErrorCode::NotFound, || {
        s.invitation_accept(&anon, &to_nobody.invitation_id)
    });
}

#[test]
fn bidi_controls_in_emails_are_refused() {
    let w = World::new();
    let s = &*w.s;
    for c in ['\u{202E}', '\u{2067}', '\u{200F}', '\u{061C}'] {
        // An invitation to such an address is refused, and nothing is stored.
        let email = format!("eve{c}@example.org");
        deny(s, ErrorCode::ValidationFailed, || {
            s.shared_invite(&w.owner, &w.space, &invite(&email, Role::Viewer, "bidi"))
        });
        // A verified email can't carry one either, so member lists never show one.
        let before = s.state_fingerprint();
        assert_eq!(
            s.set_verified_email(&w.outsider, Some(&format!("gro.elpmaxe@{c}eve"))),
            Err(penroam_sync_protocol::SharingError::Email)
        );
        assert_eq!(s.state_fingerprint(), before);
    }
    let members = s.shared_members(&w.owner, &w.space).unwrap().members;
    assert!(members.iter().all(|m| m.validate().is_ok()));
}

#[test]
fn invitations_expire_on_the_test_clock() {
    let w = World::new();
    let s = &*w.s;
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "e"),
        )
        .unwrap();
    assert_eq!(inv.expires_at_ms, T0 + INVITATION_TTL_MS);
    s.set_clock_ms(Some(T0 + INVITATION_TTL_MS - 1));
    assert_eq!(
        s.invitations_incoming(&w.outsider)
            .unwrap()
            .invitations
            .len(),
        1
    );
    s.advance_clock_ms(1);
    assert!(s
        .invitations_incoming(&w.outsider)
        .unwrap()
        .invitations
        .is_empty());
    deny(s, ErrorCode::InvitationExpired, || {
        s.invitation_accept(&w.outsider, &inv.invitation_id)
    });
    deny(s, ErrorCode::InvitationExpired, || {
        s.invitation_decline(&w.outsider, &inv.invitation_id)
    });
    deny(s, ErrorCode::Conflict, || {
        s.invitation_cancel(&w.owner, &inv.invitation_id)
    });
    let listed = s.shared_members(&w.owner, &w.space).unwrap().invitations;
    let e = listed
        .iter()
        .find(|i| i.invitation_id == inv.invitation_id)
        .unwrap();
    assert_eq!(e.state, InvitationState::Expired);
    assert_eq!(e.version, 2);
    // A new invitation works after expiry.
    let again = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "e-again"),
        )
        .unwrap();
    s.invitation_accept(&w.outsider, &again.invitation_id)
        .unwrap();
}

#[test]
fn cancel_decline_reinvite_and_accept_idempotency() {
    let w = World::new();
    let s = &*w.s;
    // Cancel: the invitee can no longer see or accept it; cancelling twice is fine.
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "c1"),
        )
        .unwrap();
    s.invitation_cancel(&w.owner, &inv.invitation_id).unwrap();
    s.invitation_cancel(&w.owner, &inv.invitation_id).unwrap();
    assert!(s
        .invitations_incoming(&w.outsider)
        .unwrap()
        .invitations
        .is_empty());
    deny(s, ErrorCode::NotFound, || {
        s.invitation_accept(&w.outsider, &inv.invitation_id)
    });
    deny(s, ErrorCode::NotFound, || {
        s.invitation_decline(&w.outsider, &inv.invitation_id)
    });

    // Decline, decline again, then a fresh invitation to the same address.
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Editor, "c2"),
        )
        .unwrap();
    s.invitation_decline(&w.outsider, &inv.invitation_id)
        .unwrap();
    s.invitation_decline(&w.outsider, &inv.invitation_id)
        .unwrap();
    deny(s, ErrorCode::Conflict, || {
        s.invitation_accept(&w.outsider, &inv.invitation_id)
    });
    deny(s, ErrorCode::Conflict, || {
        s.invitation_cancel(&w.owner, &inv.invitation_id)
    });
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("eve@example.org", Role::Viewer, "c3"),
        )
        .unwrap();

    // Accept is idempotent, also when the first response was lost.
    s.drop_next_responses(1);
    assert_eq!(
        s.invitation_accept(&w.outsider, &inv.invitation_id),
        Err(RequestError::ResponseLost)
    );
    let a = s
        .invitation_accept(&w.outsider, &inv.invitation_id)
        .unwrap();
    let b = s
        .invitation_accept(&w.outsider, &inv.invitation_id)
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.role, Role::Viewer);
    let members = s.shared_members(&w.owner, &w.space).unwrap().members;
    assert_eq!(
        members
            .iter()
            .filter(|m| m.account_id == w.outsider)
            .count(),
        1
    );
    deny(s, ErrorCode::Conflict, || {
        s.invitation_decline(&w.outsider, &inv.invitation_id)
    });
    deny(s, ErrorCode::Conflict, || {
        s.invitation_cancel(&w.owner, &inv.invitation_id)
    });
    // After removal, replaying the accept does not re-admit.
    s.shared_remove_member(&w.owner, &w.space, &w.outsider)
        .unwrap();
    deny(s, ErrorCode::AccessRevoked, || {
        s.invitation_accept(&w.outsider, &inv.invitation_id)
    });
}

#[test]
fn leaving_removing_and_rejoining() {
    let w = World::new();
    let s = &*w.s;
    // A member leaves; the owner cannot.
    s.shared_remove_member(&w.viewer, &w.space, &w.viewer)
        .unwrap();
    assert_eq!(
        code(s.shared_members(&w.viewer, &w.space)),
        ErrorCode::AccessRevoked
    );
    assert!(s.device_revoked(&w.space, &dev("dev-viewer")));
    deny(s, ErrorCode::Forbidden, || {
        s.shared_remove_member(&w.owner, &w.space, &w.owner)
    });
    // Removing an already removed member is idempotent.
    s.shared_remove_member(&w.owner, &w.space, &w.revoked)
        .unwrap();

    // The removed member is invited again: active with the new role and a new version, but the
    // old device stays revoked, so they enroll a new one.
    let inv = s
        .shared_invite(
            &w.owner,
            &w.space,
            &invite("dan@example.org", Role::Viewer, "back"),
        )
        .unwrap();
    let rec = s.invitation_accept(&w.revoked, &inv.invitation_id).unwrap();
    assert_eq!(
        (rec.role, rec.state),
        (Role::Viewer, MembershipState::Active)
    );
    let m = s
        .shared_members(&w.owner, &w.space)
        .unwrap()
        .members
        .into_iter()
        .find(|m| m.account_id == w.revoked)
        .unwrap();
    assert_eq!(m.version, 3);
    let req = PullOpsRequest {
        cursor: None,
        limit: 1,
    };
    assert_eq!(
        code(s.pull(w.m(&w.revoked), &dev("dev-revoked"), &req)),
        ErrorCode::DeviceRevoked
    );
    s.register_device_keys(w.m(&w.revoked), &reg("dev-revoked-new"))
        .unwrap();
    s.pull(w.m(&w.revoked), &dev("dev-revoked-new"), &req)
        .unwrap();
}

// ------------------------------------------------------------------ atomicity --

#[test]
fn concurrent_accept_and_cancel_have_one_winner() {
    let w = World::new();
    for round in 0..40 {
        let inv =
            w.s.shared_invite(
                &w.owner,
                &w.space,
                &invite("eve@example.org", Role::Editor, &format!("race-{round}")),
            )
            .unwrap();
        let (s1, s2) = (w.s.clone(), w.s.clone());
        let (id1, id2) = (inv.invitation_id.clone(), inv.invitation_id.clone());
        let (o, out) = (w.owner.clone(), w.outsider.clone());
        let accept = std::thread::spawn(move || s1.invitation_accept(&out, &id1));
        let cancel = std::thread::spawn(move || s2.invitation_cancel(&o, &id2));
        let (accepted, cancelled) = (accept.join().unwrap(), cancel.join().unwrap());
        let member =
            w.s.shared_members(&w.owner, &w.space)
                .unwrap()
                .members
                .into_iter()
                .any(|m| m.account_id == w.outsider && m.state == MembershipState::Active);
        match (&accepted, &cancelled) {
            (Ok(_), Err(e)) => {
                assert_eq!(e.code(), Some(ErrorCode::Conflict));
                assert!(member);
                w.s.shared_remove_member(&w.owner, &w.space, &w.outsider)
                    .unwrap();
            }
            (Err(e), Ok(())) => {
                assert_eq!(e.code(), Some(ErrorCode::NotFound));
                assert!(!member);
            }
            other => panic!("both or neither succeeded: {other:?}"),
        }
    }
}

#[test]
fn removal_cuts_off_a_member_who_is_pushing() {
    let w = World::new();
    let (s, space, editor) = (w.s.clone(), w.space.clone(), w.editor.clone());
    let pusher = std::thread::spawn(move || {
        let mut accepted = 0u64;
        for n in 1..=1_000_000 {
            let r = s.push(
                Caller::member(&space, &editor),
                &dev("dev-editor"),
                &batch(&[op("dev-editor", n)]),
            );
            match r {
                Ok(_) => accepted += 1,
                Err(e) => {
                    assert_eq!(e.code(), Some(ErrorCode::AccessRevoked));
                    return (accepted, n);
                }
            }
        }
        panic!("the removal never took effect")
    });
    // Let the pusher get going so the removal lands mid-stream.
    while w.s.op_count(&w.space) < 20 {
        std::thread::yield_now();
    }
    w.s.shared_remove_member(&w.owner, &w.space, &w.editor)
        .unwrap();
    let (accepted, _) = pusher.join().unwrap();
    assert!(accepted >= 20);
    // Everything accepted before the removal is in the log, nothing after it.
    assert_eq!(w.s.op_count(&w.space), accepted);
    assert_eq!(
        code(w.s.push(
            w.m(&w.editor),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", 999)])
        )),
        ErrorCode::AccessRevoked
    );
}

// --------------------------------------------------------- quota and persistence --

#[test]
fn space_storage_counts_towards_the_owner_quota() {
    let w = World::new();
    let s = &*w.s;
    s.set_quota(&w.owner, Some(100));
    // 2 × 40 bytes in the space, by the editor.
    for n in 1..=2 {
        s.push(
            w.m(&w.editor),
            &dev("dev-editor"),
            &batch(&[op("dev-editor", n)]),
        )
        .unwrap();
    }
    let state = s.account_state(&w.owner).unwrap();
    assert_eq!(state.storage.used_bytes, 80);
    assert_eq!(s.usage(w.m(&w.viewer)).unwrap().quota_bytes, Some(100));
    // The owner's own account is now short of room ...
    s.register_device(&w.owner, &dev("dev-own"));
    let r = s
        .push(&w.owner, &dev("dev-own"), &batch(&[op("dev-own", 1)]))
        .unwrap();
    assert!(matches!(
        &r.acks[0].result,
        AckResult::Rejected { error } if error.code == ErrorCode::QuotaExceeded
    ));
    // ... and so is the space, for every writer.
    let r = s
        .push(
            w.m(&w.owner),
            &dev("dev-owner"),
            &batch(&[op("dev-owner", 1)]),
        )
        .unwrap();
    assert!(matches!(
        &r.acks[0].result,
        AckResult::Rejected { error } if error.code == ErrorCode::QuotaExceeded
    ));
    // Members' own accounts are unaffected.
    s.register_device(&w.editor, &dev("dev-bob"));
    let r = s
        .push(&w.editor, &dev("dev-bob"), &batch(&[op("dev-bob", 1)]))
        .unwrap();
    assert!(matches!(r.acks[0].result, AckResult::Accepted { .. }));
}

#[test]
fn spaces_members_and_invitations_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (space, inv_id) = {
        let w = World::with(InMemoryServer::open_persistent(dir.path()).unwrap());
        let inv =
            w.s.shared_invite(
                &w.owner,
                &w.space,
                &invite("eve@example.org", Role::Viewer, "p"),
            )
            .unwrap();
        (w.space.clone(), inv.invitation_id)
    };
    let s = InMemoryServer::open_persistent(dir.path()).unwrap();
    let owner = acct("acct-owner");
    let members = s.shared_members(&owner, &space).unwrap();
    assert_eq!(members.members.len(), 4);
    assert_eq!(members.members[1].display, "bob@example.org");
    assert_eq!(
        code(s.shared_members(&acct("acct-revoked"), &space)),
        ErrorCode::AccessRevoked
    );
    assert_eq!(
        s.device_registered_by(&space, &dev("dev-viewer")),
        Some(acct("acct-viewer"))
    );
    let incoming = s.invitations_incoming(&acct("acct-outsider")).unwrap();
    assert_eq!(incoming.invitations[0].invitation_id, inv_id);
    let id: InvitationId = inv_id;
    s.invitation_accept(&acct("acct-outsider"), &id).unwrap();
}

#[test]
fn meta_advertises_sharing() {
    assert!(InMemoryServer::new()
        .meta()
        .has_capability(penroam_sync_protocol::dto::capabilities::SHARING_V1));
}

#[test]
fn members_have_a_device_cap_per_space() {
    let w = World::new();
    let s = &*w.s;
    s.set_member_device_cap(3);
    // The editor has dev-editor already; two more fit.
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor-2"))
        .unwrap();
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor-3"))
        .unwrap();
    deny(s, ErrorCode::Conflict, || {
        s.register_device_keys(w.m(&w.editor), &reg("dev-editor-4"))
    });
    // Re-registering a device you have is not a new one.
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor-3"))
        .unwrap();
    // The cap is per member: others are unaffected.
    s.register_device_keys(w.m(&w.viewer), &reg("dev-viewer-2"))
        .unwrap();
    // Revoked devices don't count.
    s.revoke_device_as(w.m(&w.editor), &dev("dev-editor"), &dev("dev-editor-3"))
        .unwrap();
    s.register_device_keys(w.m(&w.editor), &reg("dev-editor-4"))
        .unwrap();
    // Ordinary accounts have no cap.
    s.set_member_device_cap(0);
    for n in 0..3 {
        s.register_device_keys(&w.owner, &reg(&format!("own-{n}")))
            .unwrap();
    }
}
