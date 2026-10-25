//! Version allocation per scope (spec 2.1): versions come from the scope
//! owner's counter under a row lock held until commit.

mod common;

use std::time::Duration;

use pod_protocol::{PodcastId, UserId, normalize_feed_url};
use pod_server::version::VersionScope;
use sqlx::PgPool;

const FEED_URL: &str = "https://feeds.example.com/daily.xml";

async fn podcast_counter(pool: &PgPool, podcast_id: PodcastId) -> i64 {
    sqlx::query_scalar!(
        "select version_counter from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the podcast exists")
}

async fn user_counter(pool: &PgPool, user_id: UserId) -> i64 {
    sqlx::query_scalar!(
        "select version_counter from users where id = $1",
        user_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the user exists")
}

async fn lock_podcast(pool: &PgPool, podcast_id: PodcastId) -> VersionScope {
    VersionScope::begin_podcast(pool, podcast_id)
        .await
        .expect("the podcast row locks")
        .expect("the podcast exists")
}

async fn lock_user(pool: &PgPool, user_id: UserId) -> VersionScope {
    VersionScope::begin_user(pool, user_id)
        .await
        .expect("the user row locks")
        .expect("the user exists")
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn podcast_scope_continues_from_the_counter_and_stores_the_last_version(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;
    common::create_episodes(&pool, podcast_id, 2).await;

    let mut scope = lock_podcast(&pool, podcast_id).await;
    assert_eq!(scope.current(), 2);
    assert_eq!([scope.next_version(), scope.next_version()], [3, 4]);
    assert_eq!(scope.current(), 4);
    scope.commit().await.expect("the scope commits");

    assert_eq!(podcast_counter(&pool, podcast_id).await, 4);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn user_scope_continues_from_the_counter_and_stores_the_last_version(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;

    let mut scope = lock_user(&pool, user_id).await;
    assert_eq!(scope.current(), 0);
    assert_eq!(
        [
            scope.next_version(),
            scope.next_version(),
            scope.next_version()
        ],
        [1, 2, 3]
    );
    scope.commit().await.expect("the scope commits");

    let mut scope = lock_user(&pool, user_id).await;
    assert_eq!(scope.next_version(), 4);
    scope.commit().await.expect("the scope commits");

    assert_eq!(user_counter(&pool, user_id).await, 4);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_missing_scope_owner_is_none(pool: PgPool) {
    let url = normalize_feed_url(FEED_URL).expect("the test URL is valid");
    let podcast_id = PodcastId::from_feed_url(&url);
    let user_id = UserId::new_v7();
    assert!(
        VersionScope::begin_podcast(&pool, podcast_id)
            .await
            .expect("the lookup runs")
            .is_none()
    );
    assert!(
        VersionScope::begin_user(&pool, user_id)
            .await
            .expect("the lookup runs")
            .is_none()
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn committing_without_allocating_leaves_the_owner_row_alone(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    let row_version = || {
        sqlx::query_scalar!(
            r#"select xmin::text as "xmin!" from users where id = $1"#,
            user_id.as_uuid()
        )
        .fetch_one(&pool)
    };
    let before = row_version().await.expect("the user exists");

    let scope = lock_user(&pool, user_id).await;
    scope.commit().await.expect("the scope commits");

    // Any update, even to the same value, writes a new tuple with a new xmin.
    let after = row_version().await.expect("the user exists");
    assert_eq!(before, after);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn dropping_the_scope_rolls_back_its_rows_and_versions(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;

    let mut scope = lock_podcast(&pool, podcast_id).await;
    let version = scope.next_version();
    common::insert_episode(scope.conn(), podcast_id, version).await;
    drop(scope);

    assert_eq!(podcast_counter(&pool, podcast_id).await, 0);
    let mut scope = lock_podcast(&pool, podcast_id).await;
    assert_eq!(
        scope.next_version(),
        1,
        "the rolled-back version is handed out again"
    );
    let episodes = sqlx::query_scalar!(
        r#"select count(*) as "count!" from episode where podcast_id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(scope.conn())
    .await
    .expect("the episode count is readable");
    assert_eq!(episodes, 0);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_large_batch_gets_one_unique_version_per_row(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;
    common::create_episodes(&pool, podcast_id, 3).await;
    common::create_episodes(&pool, podcast_id, 500).await;

    let versions = sqlx::query!(
        r#"select count(*) as "rows!", count(distinct version) as "distinct!",
                  min(version) as "min!", max(version) as "max!"
           from episode where podcast_id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(&pool)
    .await
    .expect("the episodes are readable");
    assert_eq!(
        (versions.rows, versions.distinct, versions.min, versions.max),
        (503, 503, 1, 503)
    );
    assert_eq!(podcast_counter(&pool, podcast_id).await, 503);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn writers_in_one_scope_take_turns_in_version_order(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;

    let mut first = lock_podcast(&pool, podcast_id).await;
    let first_versions = [first.next_version(), first.next_version()];

    let second = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut scope = lock_podcast(&pool, podcast_id).await;
            let versions = [scope.next_version(), scope.next_version()];
            scope.commit().await.expect("the second writer commits");
            versions
        }
    });

    common::wait_for_lock_waiters(&pool, 1).await;
    assert!(
        !second.is_finished(),
        "the second writer waits for the first to commit"
    );
    first.commit().await.expect("the first writer commits");
    let second_versions = second.await.expect("the second writer doesn't panic");

    // The second writer read the counter only after the first committed, so
    // its versions continue from the first's and it commits after it.
    assert_eq!(first_versions, [1, 2]);
    assert_eq!(second_versions, [3, 4]);
    assert_eq!(podcast_counter(&pool, podcast_id).await, 4);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn writers_in_different_scopes_run_in_parallel(pool: PgPool) {
    let held_podcast = common::create_podcast(&pool, FEED_URL).await;
    let other_podcast = common::create_podcast(&pool, "https://feeds.example.com/weekly.xml").await;
    let user_id = common::create_user(&pool, "alice").await;

    let mut held = lock_podcast(&pool, held_podcast).await;
    held.next_version();

    // Both writers finish while `held` still holds its lock. If either
    // waited on it, the timeout would fire because nothing releases it.
    let others = async {
        let mut podcast = lock_podcast(&pool, other_podcast).await;
        podcast.next_version();
        podcast.commit().await.expect("the other podcast commits");
        let mut user = lock_user(&pool, user_id).await;
        user.next_version();
        user.commit().await.expect("the user scope commits");
    };
    tokio::time::timeout(Duration::from_secs(5), others)
        .await
        .expect("other scopes don't wait for the held lock");

    held.commit().await.expect("the held scope commits");
    assert_eq!(podcast_counter(&pool, held_podcast).await, 1);
    assert_eq!(podcast_counter(&pool, other_podcast).await, 1);
    assert_eq!(user_counter(&pool, user_id).await, 1);
}
