// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// db_test.rs — see module docs

// Copyright (c) 2026 Jules IMF
//! Database integration tests — create an in-memory SQLite DB.

use teststand::db;

async fn open_temp_db() -> db::Db {
    db::open_memory().await.expect("open_memory failed")
}

#[tokio::test]
async fn insert_and_list_experiments() {
    let pool = open_temp_db().await;
    db::insert_experiment(&pool, "id-1", "test", "ubuntu", "Chain", "{}")
        .await
        .unwrap();
    let list = db::list_experiments(&pool).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "test");
    assert_eq!(list[0].status, "queued");
}

#[tokio::test]
async fn update_experiment_status() {
    let pool = open_temp_db().await;
    db::insert_experiment(&pool, "id-2", "e2", "debian", "Chain", "{}")
        .await
        .unwrap();
    db::update_experiment_status(&pool, "id-2", "running")
        .await
        .unwrap();
    let exp = db::get_experiment(&pool, "id-2").await.unwrap().unwrap();
    assert_eq!(exp.status, "running");
}

#[tokio::test]
async fn append_and_get_logs() {
    let pool = open_temp_db().await;
    db::append_log(&pool, "run-1", "info", "hello")
        .await
        .unwrap();
    db::append_log(&pool, "run-1", "error", "oops")
        .await
        .unwrap();
    let logs = db::get_logs(&pool, "run-1").await.unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].level, "info");
    assert_eq!(logs[1].level, "error");
}

#[tokio::test]
async fn telegram_subscribers_crud() {
    let pool = open_temp_db().await;
    db::add_telegram_subscriber(&pool, 111).await.unwrap();
    db::add_telegram_subscriber(&pool, 222).await.unwrap();
    db::add_telegram_subscriber(&pool, 111).await.unwrap(); // duplicate — should be ignored
    let subs = db::list_telegram_subscribers(&pool).await.unwrap();
    assert_eq!(subs.len(), 2);
    db::remove_telegram_subscriber(&pool, 111).await.unwrap();
    let subs = db::list_telegram_subscribers(&pool).await.unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0], 222);
}
