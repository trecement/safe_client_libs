// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under the MIT license <LICENSE-MIT
// http://opensource.org/licenses/MIT> or the Modified BSD license <LICENSE-BSD
// https://opensource.org/licenses/BSD-3-Clause>, at your option. This file may not be copied,
// modified, or distributed except according to those terms. Please review the Licences for the
// specific language governing permissions and limitations relating to use of the SAFE Network
// Software.

use client::AppClient;
use ffi::test_utils::test_create_app;
use ffi_utils::test_utils::call_1;
use futures::Future;
use maidsafe_utilities::thread;
use rand::{OsRng, Rng};
use routing::{Action, ClientError, EntryAction, MutableData, PermissionSet, User, Value, XorName};
use rust_sodium::crypto::sign;
use safe_core::utils::test_utils::random_client;
use safe_core::{utils, Client, CoreError, FutureExt, DIR_TAG};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::sync::mpsc;
use test_utils::{create_app, run};

// MD created by App. App lists its own sign_pk in owners field: Put should fail - Rejected by
// MaidManagers. Should pass when it lists the owner's sign_pk instead.
#[test]
#[allow(unsafe_code)]
fn md_created_by_app_1() {
    use App;

    let app_id = unwrap!(utils::generate_random_string(10));
    let app_id = unwrap!(CString::new(app_id));
    let app: *mut App =
        unsafe { unwrap!(call_1(|ud, cb| test_create_app(app_id.as_ptr(), ud, cb))) };

    run(unsafe { &*app }, |client: &AppClient, _app_context| {
        let mut rng = unwrap!(OsRng::new());

        let owners = btree_set![unwrap!(client.public_signing_key())];
        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(
            name,
            DIR_TAG,
            BTreeMap::new(),
            BTreeMap::new(),
            owners,
        ));
        let cl2 = client.clone();
        client
            .put_mdata(mdata)
            .then(move |res| {
                match res {
                    Ok(()) => panic!("Put should be rejected by MaidManagers"),
                    Err(CoreError::RoutingClientError(ClientError::InvalidOwners)) => (),
                    Err(x) => panic!("Expected ClientError::InvalidOwners. Got {:?}", x),
                }
                let mut owners = BTreeSet::new();
                let _ = owners.insert(unwrap!(cl2.owner_key()));
                let mdata = unwrap!(MutableData::new(
                    name,
                    DIR_TAG,
                    BTreeMap::new(),
                    BTreeMap::new(),
                    owners,
                ));
                cl2.put_mdata(mdata)
            }).map_err(|e| panic!("{:?}", e))
    });
}

// MD created by App properly: Should pass. App tries to change ownership - Should Fail by
// MaidManagers. App creates its own account with the maid-managers. Now it tries changing ownership
// by routing it through it's MM instead of owners. It should still fail as DataManagers should
// enforce that the request is coming from MM of the owner (listed in the owners field of the stored
// MD).
#[test]
fn md_created_by_app_2() {
    let app = create_app();
    let (tx, rx) = mpsc::channel();
    let (alt_client_tx, alt_client_rx) = mpsc::channel();
    unwrap!(app.send(move |client, _app_context| {
        let mut rng = unwrap!(OsRng::new());
        let sign_pk = unwrap!(client.public_signing_key());

        let mut permissions = BTreeMap::new();
        let _ = permissions.insert(
            User::Key(sign_pk),
            PermissionSet::new().allow(Action::ManagePermissions),
        );

        let owners = btree_set![unwrap!(client.owner_key())];

        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(
            name,
            DIR_TAG,
            permissions,
            BTreeMap::new(),
            owners,
        ));
        let name2 = name;
        let cl2 = client.clone();
        client
            .put_mdata(mdata)
            .then(move |res| {
                unwrap!(res);
                cl2.change_mdata_owner(name, DIR_TAG, sign_pk, 1)
            }).then(move |res| -> Result<_, ()> {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                unwrap!(alt_client_tx.send((name2, sign_pk)));
                Ok(())
            }).into_box()
            .into()
    }));
    let _joiner = thread::named("Alt client", || {
        random_client(move |client| {
            let (name, sign_pk) = unwrap!(alt_client_rx.recv());
            let cl2 = client.clone();
            let cl3 = client.clone();
            client
                .list_auth_keys_and_version()
                .then(move |res| {
                    let (_, version) = unwrap!(res);
                    cl2.ins_auth_key(sign_pk, version + 1)
                }).then(move |res| {
                    unwrap!(res);
                    cl3.change_mdata_owner(name, DIR_TAG, sign_pk, 1)
                }).then(move |res| -> Result<(), ()> {
                    match res {
                        Ok(()) => panic!("It should fail"),
                        Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                        Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                    }
                    unwrap!(tx.send(()));
                    Ok(())
                })
        });
    });
    unwrap!(rx.recv());
}

// MD created by owner and given to a permitted App. Owner has listed that app is allowed to insert
// only. App tries to insert - should pass. App tries to update - should fail. App tries to change
// permission to allow itself to update - should fail to change permissions.
#[test]
fn md_created_by_app_3() {
    let app = create_app();
    let (tx, rx) = mpsc::channel();
    let (app_sign_pk_tx, app_sign_pk_rx) = mpsc::channel();
    let (name_tx, name_rx) = mpsc::channel();
    unwrap!(app.send(move |client, _app_context| {
        let sign_pk = unwrap!(client.public_signing_key());
        unwrap!(app_sign_pk_tx.send(sign_pk));
        let name: XorName = unwrap!(name_rx.recv());
        let mut actions = BTreeMap::new();
        let _ = actions.insert(
            vec![1, 2, 3, 4],
            EntryAction::Ins(Value {
                content: vec![2, 3, 5],
                entry_version: 1,
            }),
        );
        let cl2 = client.clone();
        let cl3 = client.clone();
        let name2 = name;
        client
            .mutate_mdata_entries(name, DIR_TAG, actions)
            .then(move |res| {
                unwrap!(res);
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![1, 2, 3, 4],
                    EntryAction::Update(Value {
                        content: vec![2, 8, 5],
                        entry_version: 2,
                    }),
                );
                cl2.mutate_mdata_entries(name, DIR_TAG, actions)
            }).then(move |res| {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                let user = User::Key(sign_pk);
                let permissions = PermissionSet::new().allow(Action::Update);
                cl3.set_mdata_user_permissions(name2, DIR_TAG, user, permissions, 2)
            }).then(move |res| -> Result<_, ()> {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                unwrap!(tx.send(()));
                Ok(())
            }).into_box()
            .into()
    }));
    let _joiner = thread::named("Alt client", || {
        random_client(move |client| {
            let app_sign_pk = unwrap!(app_sign_pk_rx.recv());
            let mut rng = unwrap!(OsRng::new());

            let mut permissions = BTreeMap::new();
            let _ = permissions.insert(
                User::Key(app_sign_pk),
                PermissionSet::new().allow(Action::Insert),
            );

            let mut owners = BTreeSet::new();
            let _ = owners.insert(unwrap!(client.owner_key()));

            let name: XorName = rng.gen();

            let mdata = unwrap!(MutableData::new(
                name,
                DIR_TAG,
                permissions,
                BTreeMap::new(),
                owners,
            ));
            let cl2 = client.clone();
            let cl3 = client.clone();

            client
                .list_auth_keys_and_version()
                .then(move |res| {
                    let (_, version) = unwrap!(res);
                    cl2.ins_auth_key(app_sign_pk, version + 1)
                }).then(move |res| {
                    unwrap!(res);
                    cl3.put_mdata(mdata)
                }).map(move |()| unwrap!(name_tx.send(name)))
                .map_err(|e| panic!("{:?}", e))
        });
    });
    unwrap!(rx.recv());
}

// MD created by owner and given to a permitted App. Owner has listed that app is allowed to
// manage-permissions only. App tries to insert - should fail. App tries to update - should fail.
// App tries to change permission to allow itself to insert and delete - should pass to change
// permissions. Now App tires to insert again - should pass. App tries to update. Should fail. App
// tries to delete - should pass.
#[test]
fn md_created_by_app_4() {
    let app = create_app();
    let (tx, rx) = mpsc::channel();
    let (app_sign_pk_tx, app_sign_pk_rx) = mpsc::channel();
    let (name_tx, name_rx) = mpsc::channel();
    unwrap!(app.send(move |client, _app_context| {
        let sign_pk = unwrap!(client.public_signing_key());
        unwrap!(app_sign_pk_tx.send(sign_pk));
        let name: XorName = unwrap!(name_rx.recv());
        let mut actions = BTreeMap::new();
        let _ = actions.insert(
            vec![1, 2, 3, 4],
            EntryAction::Ins(Value {
                content: vec![2, 3, 5],
                entry_version: 1,
            }),
        );
        let cl2 = client.clone();
        let cl3 = client.clone();
        let cl4 = client.clone();
        let cl5 = client.clone();
        let cl6 = client.clone();
        let name2 = name;
        let name3 = name;
        let name4 = name;
        let name5 = name;
        client
            .mutate_mdata_entries(name, DIR_TAG, actions)
            .then(move |res| {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![1, 8, 3, 4],
                    EntryAction::Update(Value {
                        content: vec![2, 8, 5],
                        entry_version: 2,
                    }),
                );
                cl2.mutate_mdata_entries(name, DIR_TAG, actions)
            }).then(move |res| {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                let user = User::Key(sign_pk);
                let permissions = PermissionSet::new()
                    .allow(Action::Insert)
                    .allow(Action::Delete);
                cl3.set_mdata_user_permissions(name2, DIR_TAG, user, permissions, 1)
            }).then(move |res| {
                unwrap!(res);
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![1, 2, 3, 4],
                    EntryAction::Ins(Value {
                        content: vec![2, 3, 5],
                        entry_version: 1,
                    }),
                );
                cl4.mutate_mdata_entries(name3, DIR_TAG, actions)
            }).then(move |res| {
                unwrap!(res);
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![1, 2, 3, 4],
                    EntryAction::Update(Value {
                        content: vec![2, 8, 5],
                        entry_version: 2,
                    }),
                );
                cl5.mutate_mdata_entries(name4, DIR_TAG, actions)
            }).then(move |res| {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                let mut actions = BTreeMap::new();
                let _ = actions.insert(vec![1, 2, 3, 4], EntryAction::Del(2));
                cl6.mutate_mdata_entries(name5, DIR_TAG, actions)
            }).map(move |()| unwrap!(tx.send(())))
            .map_err(|e| panic!("{:?}", e))
            .into_box()
            .into()
    }));
    let _joiner = thread::named("Alt client", || {
        random_client(move |client| {
            let app_sign_pk = unwrap!(app_sign_pk_rx.recv());
            let mut rng = unwrap!(OsRng::new());

            let mut permissions = BTreeMap::new();
            let _ = permissions.insert(
                User::Key(app_sign_pk),
                PermissionSet::new().allow(Action::ManagePermissions),
            );

            let mut data = BTreeMap::new();
            let _ = data.insert(
                vec![1, 8, 3, 4],
                Value {
                    content: vec![1],
                    entry_version: 1,
                },
            );

            let mut owners = BTreeSet::new();
            let _ = owners.insert(unwrap!(client.owner_key()));

            let name: XorName = rng.gen();

            let mdata = unwrap!(MutableData::new(name, DIR_TAG, permissions, data, owners));
            let cl2 = client.clone();
            let cl3 = client.clone();

            client
                .list_auth_keys_and_version()
                .then(move |res| {
                    let (_, version) = unwrap!(res);
                    cl2.ins_auth_key(app_sign_pk, version + 1)
                }).then(move |res| {
                    unwrap!(res);
                    cl3.put_mdata(mdata)
                }).map(move |()| unwrap!(name_tx.send(name)))
                .map_err(|e| panic!("{:?}", e))
        });
    });
    unwrap!(rx.recv());
}

// MD created by App1, with permission to insert by anyone and permission to manage-permissions only
// for itself - should pass. App2 created via another random client2 tries to insert (going via
// client2's MM) into MD of App1 - should Pass. App1 should be able to read the data - should pass.
// App1 changes permission to remove the anyone access - should pass. App2 tries to insert another
// data in MD - should fail. App1 tries to get all data from MD - should pass and should have no
// change (since App2 failed to insert).
#[test]
fn multiple_apps() {
    let app1 = create_app();
    let app2 = create_app();
    let (tx, rx) = mpsc::channel();
    let (name_tx, name_rx) = mpsc::channel();
    let (entry_tx, entry_rx) = mpsc::channel();
    let (mutate_again_tx, mutate_again_rx) = mpsc::channel();
    let (final_check_tx, final_check_rx) = mpsc::channel();
    unwrap!(app1.send(move |client, _app_context| {
        let mut rng = unwrap!(OsRng::new());
        let sign_pk = unwrap!(client.public_signing_key());

        let mut permissions = BTreeMap::new();
        let _ = permissions.insert(User::Anyone, PermissionSet::new().allow(Action::Insert));
        let _ = permissions.insert(
            User::Key(sign_pk),
            PermissionSet::new().allow(Action::ManagePermissions),
        );

        let mut owners = BTreeSet::new();
        let _ = owners.insert(unwrap!(client.owner_key()));

        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(
            name,
            DIR_TAG,
            permissions,
            BTreeMap::new(),
            owners,
        ));
        let cl2 = client.clone();
        let cl3 = client.clone();
        let cl4 = client.clone();
        let name2 = name;
        let name3 = name;
        client
            .put_mdata(mdata)
            .then(move |res| {
                unwrap!(res);
                unwrap!(name_tx.send(name));
                let entry_key: Vec<u8> = unwrap!(entry_rx.recv());
                cl2.get_mdata_value(name, DIR_TAG, entry_key.clone())
                    .map(move |v| (v, entry_key))
            }).then(move |res| {
                let (value, entry_key) = unwrap!(res);
                assert_eq!(
                    value,
                    Value {
                        content: vec![8],
                        entry_version: 1,
                    }
                );
                cl3.del_mdata_user_permissions(name2, DIR_TAG, User::Anyone, 1)
                    .map(move |()| entry_key)
            }).then(move |res| {
                let entry_key = unwrap!(res);
                unwrap!(mutate_again_tx.send(()));
                unwrap!(final_check_rx.recv());
                cl4.list_mdata_keys(name3, DIR_TAG)
                    .map(move |x| (x, entry_key))
            }).then(move |res| -> Result<_, ()> {
                let (keys, entry_key) = unwrap!(res);
                assert_eq!(keys.len(), 1);
                assert!(keys.contains(&entry_key));
                unwrap!(tx.send(()));
                Ok(())
            }).into_box()
            .into()
    }));
    unwrap!(app2.send(move |client, _app_context| {
        let name = unwrap!(name_rx.recv());
        let entry_key = vec![1, 2, 3];

        let mut actions = BTreeMap::new();
        let _ = actions.insert(
            entry_key.clone(),
            EntryAction::Ins(Value {
                content: vec![8],
                entry_version: 1,
            }),
        );

        let cl2 = client.clone();
        client
            .mutate_mdata_entries(name, DIR_TAG, actions)
            .then(move |res| {
                unwrap!(res);
                unwrap!(entry_tx.send(entry_key));
                unwrap!(mutate_again_rx.recv());

                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![2, 2, 2],
                    EntryAction::Ins(Value {
                        content: vec![21],
                        entry_version: 1,
                    }),
                );

                cl2.mutate_mdata_entries(name, DIR_TAG, actions)
            }).then(move |res| -> Result<_, ()> {
                match res {
                    Ok(()) => panic!("It should fail"),
                    Err(CoreError::RoutingClientError(ClientError::AccessDenied)) => (),
                    Err(x) => panic!("Expected ClientError::AccessDenied. Got {:?}", x),
                }
                unwrap!(final_check_tx.send(()));
                Ok(())
            }).into_box()
            .into()
    }));
    unwrap!(rx.recv());
}

// MD created by App with itself allowed to manage-permissions. Insert permission to allow a
// random-key to perform update operation - should pass. Delete this permission without incrementing
// version of MD - should fail version check. Query the permissions list - should continue to have
// the listed permission for the random-key. Query the version of the MD in network - should pass.
// Send request to delete that permission again with properly incremented version from info from the
// fetched version - should pass. Query the permissions list - should no longer have the listed
// permission for the random-key.
#[test]
fn permissions_and_version() {
    let app = create_app();
    run(&app, |client: &AppClient, _app_context| {
        let mut rng = unwrap!(OsRng::new());
        let sign_pk = unwrap!(client.public_signing_key());
        let (random_key, _) = sign::gen_keypair();

        let mut permissions = BTreeMap::new();
        let _ = permissions.insert(
            User::Key(sign_pk),
            PermissionSet::new().allow(Action::ManagePermissions),
        );

        let mut owners = BTreeSet::new();
        let _ = owners.insert(unwrap!(client.owner_key()));

        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(
            name,
            DIR_TAG,
            permissions,
            BTreeMap::new(),
            owners,
        ));
        let cl2 = client.clone();
        let cl3 = client.clone();
        let cl4 = client.clone();
        let cl5 = client.clone();
        let cl6 = client.clone();
        let cl7 = client.clone();
        client
            .put_mdata(mdata)
            .then(move |res| {
                unwrap!(res);
                let permissions = PermissionSet::new().allow(Action::Update);
                cl2.set_mdata_user_permissions(name, DIR_TAG, User::Key(random_key), permissions, 1)
            }).then(move |res| {
                unwrap!(res);
                cl3.del_mdata_user_permissions(name, DIR_TAG, User::Key(random_key), 1)
            }).then(move |res| {
                match res {
                    Ok(()) => panic!("It should fail with invalid successor"),
                    Err(CoreError::RoutingClientError(ClientError::InvalidSuccessor(.))) => (),
                    Err(x) => panic!("Expected ClientError::InvalidSuccessor. Got {:?}", x),
                }
                cl4.list_mdata_permissions(name, DIR_TAG)
            }).then(move |res| {
                let permissions = unwrap!(res);
                assert_eq!(permissions.len(), 2);
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk)))
                        .is_allowed(Action::ManagePermissions),
                    Some(true)
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(random_key))).is_allowed(Action::Insert),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(random_key))).is_allowed(Action::Update),
                    Some(true)
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(random_key))).is_allowed(Action::Delete),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(random_key)))
                        .is_allowed(Action::ManagePermissions),
                    None
                );
                cl5.get_mdata_version(name, DIR_TAG)
            }).then(move |res| {
                let v = unwrap!(res);
                assert_eq!(v, 1);
                cl6.del_mdata_user_permissions(name, DIR_TAG, User::Key(random_key), v + 1)
            }).then(move |res| {
                unwrap!(res);
                cl7.list_mdata_permissions(name, DIR_TAG)
            }).map(move |permissions| {
                assert_eq!(permissions.len(), 1);
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                    None
                );
                assert_eq!(
                    unwrap!(permissions.get(&User::Key(sign_pk)))
                        .is_allowed(Action::ManagePermissions),
                    Some(true)
                );
            }).map_err(|e| panic!("{:?}", e))
    });
}

// The usual test to insert, update, delete and list all permissions. Put in some permissions, fetch
// (list) all of them, add some more, list again, delete one or two, list again - all should pass
// and do the expected (i.e. after list do assert that it contains all the expected stuff, don't
// just pass test if the list was successful).
#[test]
fn permissions_crud() {
    let app = create_app();
    run(&app, |client: &AppClient, _app_context| {
        let mut rng = unwrap!(OsRng::new());
        let sign_pk = unwrap!(client.public_signing_key());
        let (random_key_a, _) = sign::gen_keypair();
        let (random_key_b, _) = sign::gen_keypair();

        let mut permissions = BTreeMap::new();
        let _ = permissions.insert(
            User::Key(sign_pk),
            PermissionSet::new().allow(Action::ManagePermissions),
        );

        let mut owners = BTreeSet::new();
        let _ = owners.insert(unwrap!(client.owner_key()));

        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(
            name,
            DIR_TAG,
            permissions,
            BTreeMap::new(),
            owners,
        ));

        let cl2 = client.clone();
        let cl3 = client.clone();
        let cl4 = client.clone();
        let cl5 = client.clone();
        let cl6 = client.clone();
        let cl7 = client.clone();
        let cl8 = client.clone();
        let cl9 = client.clone();
        let cl10 = client.clone();
        client
            .put_mdata(mdata)
            .then(move |res| {
                unwrap!(res);
                let permissions = PermissionSet::new()
                    .allow(Action::Insert)
                    .allow(Action::Delete);
                cl2.set_mdata_user_permissions(
                    name,
                    DIR_TAG,
                    User::Key(random_key_a),
                    permissions,
                    1,
                )
            }).then(move |res| {
                unwrap!(res);
                cl3.list_mdata_permissions(name, DIR_TAG)
            }).then(move |res| {
                {
                    let permissions = unwrap!(res);
                    assert_eq!(permissions.len(), 2);
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk)))
                            .is_allowed(Action::ManagePermissions),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Insert),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Delete),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::ManagePermissions),
                        None
                    );
                }

                let permissions = PermissionSet::new().deny(Action::Insert);
                cl4.set_mdata_user_permissions(
                    name,
                    DIR_TAG,
                    User::Key(random_key_b),
                    permissions,
                    2,
                )
            }).then(move |res| {
                unwrap!(res);
                cl5.list_mdata_permissions(name, DIR_TAG)
            }).then(move |res| {
                {
                    let permissions = unwrap!(res);
                    assert_eq!(permissions.len(), 3);
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk)))
                            .is_allowed(Action::ManagePermissions),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Insert),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::Delete),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_a)))
                            .is_allowed(Action::ManagePermissions),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Insert),
                        Some(false)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::ManagePermissions),
                        None
                    );
                }

                let permissions = PermissionSet::new().deny(Action::Insert);
                cl6.set_mdata_user_permissions(
                    name,
                    DIR_TAG,
                    User::Key(random_key_b),
                    permissions,
                    3,
                )
            }).then(move |res| {
                unwrap!(res);
                cl7.del_mdata_user_permissions(name, DIR_TAG, User::Key(random_key_a), 4)
            }).then(move |res| {
                unwrap!(res);
                cl8.list_mdata_permissions(name, DIR_TAG)
            }).then(move |res| {
                {
                    let permissions = unwrap!(res);
                    assert_eq!(permissions.len(), 2);
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk)))
                            .is_allowed(Action::ManagePermissions),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Insert),
                        Some(false)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::ManagePermissions),
                        None
                    );
                }

                let permissions = PermissionSet::new()
                    .deny(Action::Insert)
                    .deny(Action::Delete);
                cl9.set_mdata_user_permissions(
                    name,
                    DIR_TAG,
                    User::Key(random_key_b),
                    permissions,
                    5,
                )
            }).then(move |res| {
                unwrap!(res);
                cl10.list_mdata_permissions(name, DIR_TAG)
            }).then(move |res| -> Result<_, ()> {
                {
                    let permissions = unwrap!(res);
                    assert_eq!(permissions.len(), 2);
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Insert),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk))).is_allowed(Action::Delete),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(sign_pk)))
                            .is_allowed(Action::ManagePermissions),
                        Some(true)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Insert),
                        Some(false)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Update),
                        None
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::Delete),
                        Some(false)
                    );
                    assert_eq!(
                        unwrap!(permissions.get(&User::Key(random_key_b)))
                            .is_allowed(Action::ManagePermissions),
                        None
                    );
                }

                Ok(())
            }).map_err(|e| panic!("{:?}", e))
    });
}

// The usual test to insert, update, delete and list all entry-keys/values. Same thing from
// `permissions_crud` with entry-key/value - the difference is that after delete you should still
// get all the keys - delete does not actually delete the entry, only blanks out the entry-value
// (null vector), the version however should have been bumped - so check for those.
#[test]
fn entries_crud() {
    let app = create_app();
    run(&app, |client: &AppClient, _app_context| {
        let mut rng = unwrap!(OsRng::new());
        let sign_pk = unwrap!(client.public_signing_key());

        let mut permissions = BTreeMap::new();
        let _ = permissions.insert(
            User::Key(sign_pk),
            PermissionSet::new()
                .allow(Action::Insert)
                .allow(Action::Update)
                .allow(Action::Delete),
        );

        let mut data = BTreeMap::new();
        let _ = data.insert(
            vec![0, 0, 1],
            Value {
                content: vec![1],
                entry_version: 1,
            },
        );
        let _ = data.insert(
            vec![0, 1, 0],
            Value {
                content: vec![2, 8],
                entry_version: 1,
            },
        );

        let mut owners = BTreeSet::new();
        let _ = owners.insert(unwrap!(client.owner_key()));

        let name: XorName = rng.gen();
        let mdata = unwrap!(MutableData::new(name, DIR_TAG, permissions, data, owners));

        let cl2 = client.clone();
        let cl3 = client.clone();
        let cl4 = client.clone();
        let cl5 = client.clone();
        client
            .put_mdata(mdata)
            .then(move |res| {
                unwrap!(res);
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![0, 1, 1],
                    EntryAction::Ins(Value {
                        content: vec![2, 3, 17],
                        entry_version: 1,
                    }),
                );
                let _ = actions.insert(
                    vec![0, 1, 0],
                    EntryAction::Update(Value {
                        content: vec![2, 8, 64],
                        entry_version: 2,
                    }),
                );
                let _ = actions.insert(vec![0, 0, 1], EntryAction::Del(2));
                cl2.mutate_mdata_entries(name, DIR_TAG, actions)
            }).then(move |res| {
                unwrap!(res);
                cl3.list_mdata_entries(name, DIR_TAG)
            }).then(move |res| {
                let entries = unwrap!(res);
                assert_eq!(entries.len(), 3);
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 0, 1])),
                    Value {
                        content: vec![],
                        entry_version: 2,
                    }
                );
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 1, 0])),
                    Value {
                        content: vec![2, 8, 64],
                        entry_version: 2,
                    }
                );
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 1, 1])),
                    Value {
                        content: vec![2, 3, 17],
                        entry_version: 1,
                    }
                );
                let mut actions = BTreeMap::new();
                let _ = actions.insert(
                    vec![1, 0, 0],
                    EntryAction::Ins(Value {
                        content: vec![4, 4, 4, 4],
                        entry_version: 1,
                    }),
                );
                let _ = actions.insert(
                    vec![0, 1, 0],
                    EntryAction::Update(Value {
                        content: vec![64, 8, 1],
                        entry_version: 3,
                    }),
                );
                let _ = actions.insert(vec![0, 1, 1], EntryAction::Del(2));
                cl4.mutate_mdata_entries(name, DIR_TAG, actions)
            }).then(move |res| {
                unwrap!(res);
                cl5.list_mdata_entries(name, DIR_TAG)
            }).then(|res| -> Result<_, ()> {
                let entries = unwrap!(res);
                assert_eq!(entries.len(), 4);
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 0, 1])),
                    Value {
                        content: vec![],
                        entry_version: 2,
                    }
                );
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 1, 0])),
                    Value {
                        content: vec![64, 8, 1],
                        entry_version: 3,
                    }
                );
                assert_eq!(
                    *unwrap!(entries.get(&vec![0, 1, 1])),
                    Value {
                        content: vec![],
                        entry_version: 2,
                    }
                );
                assert_eq!(
                    *unwrap!(entries.get(&vec![1, 0, 0])),
                    Value {
                        content: vec![4, 4, 4, 4],
                        entry_version: 1,
                    }
                );
                Ok(())
            }).map_err(|e| panic!("{:?}", e))
    });
}
