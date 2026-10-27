//! The migrations build the schema of spec section 2.
//!
//! The snapshot lists every column, constraint and index, so any schema
//! change shows up in review as a snapshot diff. The targeted tests below pin
//! the facts later code relies on, so they fail even if someone accepts a
//! snapshot without reading it.

use sqlx::PgPool;

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn schema_matches_snapshot(pool: PgPool) {
    insta::assert_snapshot!(describe_schema(&pool).await);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn migrating_again_is_a_no_op(pool: PgPool) {
    let before = describe_schema(&pool).await;
    pod_server::db::migrate(&pool)
        .await
        .expect("an up-to-date database migrates without error");
    assert_eq!(describe_schema(&pool).await, before);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn every_user_scope_table_is_indexed_by_version(pool: PgPool) {
    let indexes = index_definitions(&pool).await;
    for table in ["subscription", "episode_state", "user_setting"] {
        let expected = format!("ON public.{table} USING btree (user_id, version)");
        assert!(
            indexes.iter().any(|index| index.ends_with(&expected)),
            "{table} lacks a (user_id, version) index: {indexes:#?}"
        );
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn episode_indexes_serve_sync_and_listing(pool: PgPool) {
    let indexes = index_definitions(&pool).await;
    for expected in [
        "ON public.episode USING btree (podcast_id, version)",
        "ON public.episode USING btree (podcast_id, published_at DESC)",
        "UNIQUE INDEX episode_podcast_id_guid_key ON public.episode USING btree (podcast_id, guid)",
        "ON public.feed_poll USING btree (next_poll_at)",
    ] {
        assert!(
            indexes.iter().any(|index| index.contains(expected)),
            "missing {expected}: {indexes:#?}"
        );
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn podcast_status_is_checked(pool: PgPool) {
    // `with_reason` fills `failing_since` and `feed_error`, which exactly a
    // failing row must carry.
    let insert = |status: &'static str, with_reason: bool| {
        sqlx::query(
            "insert into podcast (id, feed_url, status, failing_since, feed_error, version)
             values (gen_random_uuid(), 'https://example.com/feed.xml', $1,
                     case when $2 then now() end,
                     case when $2 then '{\"code\": \"timeout\"}'::jsonb end, 0)",
        )
        .bind(status)
        .bind(with_reason)
        .execute(&pool)
    };
    let constraint = |error: sqlx::Error| {
        error
            .as_database_error()
            .and_then(|error| error.constraint())
            .map(str::to_owned)
    };
    for (status, with_reason) in [("pending", false), ("ok", false), ("failing", true), ("gone", false)]
    {
        insert(status, with_reason)
            .await
            .expect("spec status values are allowed");
    }
    let error = insert("paused", false)
        .await
        .expect_err("unknown status values are rejected");
    assert_eq!(constraint(error).as_deref(), Some("podcast_status_check"));
    let error = insert("failing", false)
        .await
        .expect_err("a failing podcast without a reason is rejected");
    assert_eq!(
        constraint(error).as_deref(),
        Some("podcast_failing_carries_reason")
    );
    let error = insert("ok", true)
        .await
        .expect_err("an ok podcast with a reason is rejected");
    assert_eq!(
        constraint(error).as_deref(),
        Some("podcast_failing_carries_reason")
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscriptions_may_name_podcasts_without_a_row(pool: PgPool) {
    // Alias redirects (spec 4.4) write a row for the old podcast id, which
    // never had a podcast row behind it.
    let user_id = sqlx::query_scalar::<_, sqlx::types::Uuid>(
        "insert into users (id, username, password_hash)
         values (gen_random_uuid(), 'alice', 'x') returning id",
    )
    .fetch_one(&pool)
    .await
    .expect("a fresh user inserts");
    sqlx::query(
        "insert into subscription (user_id, podcast_id, feed_url, subscribed,
             subscribed_at, subscribed_dev, replaced_by, version)
         values ($1, gen_random_uuid(), 'https://old.example.com/feed', false,
             now(), gen_random_uuid(), gen_random_uuid(), 1)",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("subscription has no foreign key to podcast");
}

// ==============================================================================
// Introspection
// ==============================================================================

async fn index_definitions(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select indexdef from pg_indexes where schemaname = 'public'")
        .fetch_all(pool)
        .await
        .expect("pg_indexes is readable")
}

/// Renders every table's columns (type, nullability, default), constraints
/// and indexes, one line each, in a stable order. sqlx's own bookkeeping
/// table is left out.
async fn describe_schema(pool: &PgPool) -> String {
    let columns: Vec<(String, String, String, bool, Option<String>)> = sqlx::query_as(
        "select c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod),
                a.attnotnull, pg_get_expr(d.adbin, d.adrelid)
         from pg_attribute a
         join pg_class c on c.oid = a.attrelid
         join pg_namespace n on n.oid = c.relnamespace
         left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum
         where n.nspname = 'public' and c.relkind = 'r' and a.attnum > 0
           and not a.attisdropped and c.relname <> '_sqlx_migrations'
         order by c.relname, a.attnum",
    )
    .fetch_all(pool)
    .await
    .expect("pg_attribute is readable");

    let constraints: Vec<(String, String, String)> = sqlx::query_as(
        "select conrelid::regclass::text, conname::text, pg_get_constraintdef(oid)
         from pg_constraint
         where connamespace = 'public'::regnamespace
           and conrelid <> '_sqlx_migrations'::regclass
         order by 1, 2",
    )
    .fetch_all(pool)
    .await
    .expect("pg_constraint is readable");

    let indexes: Vec<(String, String)> = sqlx::query_as(
        "select tablename::text, indexdef from pg_indexes
         where schemaname = 'public' and tablename <> '_sqlx_migrations'
         order by tablename, indexname",
    )
    .fetch_all(pool)
    .await
    .expect("pg_indexes is readable");

    let mut out = String::new();
    for (table, column, ty, not_null, default) in columns {
        let not_null = if not_null { " not null" } else { "" };
        let default = default.map(|d| format!(" default {d}")).unwrap_or_default();
        out.push_str(&format!(
            "column {table}.{column} {ty}{not_null}{default}\n"
        ));
    }
    for (table, name, definition) in constraints {
        out.push_str(&format!("constraint {table}.{name} {definition}\n"));
    }
    for (table, definition) in indexes {
        out.push_str(&format!("index {table} {definition}\n"));
    }
    out
}
