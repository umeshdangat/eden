/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{Context, Error, Result};
use blame::BlameRoot;
use blobrepo::BlobRepo;
use blobrepo_errors::*;
use blobstore::Blobstore;
use blobstore_factory::{make_blobstore, make_metadata_sql_factory, MetadataSqlFactory};
use bonsai_git_mapping::SqlBonsaiGitMappingConnection;
use bonsai_globalrev_mapping::SqlBonsaiGlobalrevMapping;
use bonsai_hg_mapping::{CachingBonsaiHgMapping, SqlBonsaiHgMapping};
use bookmarks::{Bookmarks, CachedBookmarks};
use cacheblob::{
    new_cachelib_blobstore_no_lease, new_memcache_blobstore, InProcessLease, MemcacheOps,
};
use changeset_info::ChangesetInfo;
use changesets::{CachingChangesets, SqlChangesets};
use cloned::cloned;
use dbbookmarks::SqlBookmarks;
use deleted_files_manifest::RootDeletedManifestId;
use derived_data::BonsaiDerived;
use derived_data_filenodes::FilenodesOnlyPublic;
use failure_ext::FutureFailureErrorExt;
use fastlog::RootFastlog;
use fbinit::FacebookInit;
use filenodes::Filenodes;
use filestore::FilestoreConfig;
use fsnodes::RootFsnodeId;
use futures::compat::Future01CompatExt;
use futures_ext::{try_boxfuture, BoxFuture, FutureExt};
use futures_old::{future::IntoFuture, Future};
use git_types::TreeHandle;
use maplit::btreeset;
use memblob::EagerMemblob;
use metaconfig_types::{
    self, DerivedDataConfig, FilestoreParams, Redaction, RepoConfig, StorageConfig, UnodeVersion,
};
use mononoke_types::RepositoryId;
use newfilenodes::NewFilenodesBuilder;
use phases::SqlPhasesFactory;
use readonlyblob::ReadOnlyBlobstore;
use redactedblobstore::SqlRedactedContentStore;
use repo_blobstore::RepoBlobstoreArgs;
use scuba_ext::{ScubaSampleBuilder, ScubaSampleBuilderExt};
use slog::Logger;
use sql::{rusqlite::Connection as SqliteConnection, Connection};
use sql_construct::SqlConstruct;
use sql_ext::{facebook::MysqlOptions, SqlConnections};
use std::{collections::HashMap, iter::FromIterator, sync::Arc, time::Duration};
use unodes::RootUnodeManifestId;

pub use blobstore_factory::{BlobstoreOptions, ReadOnlyStorage};

#[derive(Copy, Clone, PartialEq)]
pub enum Caching {
    Enabled,
    Disabled,
    CachelibOnlyBlobstore,
}

const BLOBSTORE_BLOBS_CACHE_POOL: &'static str = "blobstore-blobs";
const BLOBSTORE_PRESENCE_CACHE_POOL: &'static str = "blobstore-presence";

pub struct BlobrepoBuilder<'a> {
    fb: FacebookInit,
    reponame: String,
    storage_config: StorageConfig,
    repoid: RepositoryId,
    mysql_options: MysqlOptions,
    caching: Caching,
    bookmarks_cache_ttl: Option<Duration>,
    redaction: Redaction,
    scuba_censored_table: Option<String>,
    filestore_params: Option<FilestoreParams>,
    readonly_storage: ReadOnlyStorage,
    blobstore_options: BlobstoreOptions,
    logger: &'a Logger,
    derived_data_config: DerivedDataConfig,
}

impl<'a> BlobrepoBuilder<'a> {
    pub fn new(
        fb: FacebookInit,
        reponame: String,
        config: &RepoConfig,
        mysql_options: MysqlOptions,
        caching: Caching,
        scuba_censored_table: Option<String>,
        readonly_storage: ReadOnlyStorage,
        blobstore_options: BlobstoreOptions,
        logger: &'a Logger,
    ) -> Self {
        Self {
            fb,
            reponame,
            storage_config: config.storage_config.clone(),
            repoid: config.repoid,
            mysql_options,
            caching,
            bookmarks_cache_ttl: config.bookmarks_cache_ttl.clone(),
            redaction: config.redaction.clone(),
            scuba_censored_table,
            filestore_params: config.filestore.clone(),
            readonly_storage,
            blobstore_options,
            logger,
            derived_data_config: config.derived_data_config.clone(),
        }
    }

    pub fn set_redaction(&mut self, redaction: Redaction) {
        self.redaction = redaction;
    }

    /// remote (ie, MySQL), then it configures a full set of caches. Otherwise with local storage
    /// it's assumed to be a test configuration.
    ///
    /// The blobstore config is actually orthogonal to this, but it wouldn't make much sense to
    /// configure a local blobstore with a remote db, or vice versa. There's no error checking
    /// at this level (aside from disallowing a multiplexed blobstore with a local db).
    pub async fn build(self) -> Result<BlobRepo, Error> {
        let BlobrepoBuilder {
            fb,
            reponame,
            storage_config,
            repoid,
            mysql_options,
            caching,
            bookmarks_cache_ttl,
            redaction,
            scuba_censored_table,
            filestore_params,
            readonly_storage,
            blobstore_options,
            logger,
            derived_data_config,
        } = self;

        let sql_factory = make_metadata_sql_factory(
            fb,
            storage_config.metadata,
            mysql_options,
            readonly_storage,
            // FIXME: remove clone when make_metadata_sql_factory is async-await
            logger.clone(),
        )
        .boxify();

        let blobstore = make_blobstore(
            fb,
            storage_config.blobstore,
            mysql_options,
            readonly_storage,
            blobstore_options,
            // FIXME: remove clone when make_blobstore is async-await
            logger.clone(),
        )
        .boxify();

        open_blobrepo_given_datasources(
            fb,
            blobstore,
            sql_factory,
            repoid,
            caching,
            bookmarks_cache_ttl,
            redaction,
            scuba_censored_table,
            filestore_params,
            readonly_storage,
            derived_data_config,
            reponame,
        )
        .compat()
        .await
    }
}

/// Expose for graph walker that has storage open already
pub fn open_blobrepo_given_datasources(
    fb: FacebookInit,
    unredacted_blobstore: BoxFuture<Arc<dyn Blobstore>, Error>,
    sql_factory: BoxFuture<MetadataSqlFactory, Error>,
    repoid: RepositoryId,
    caching: Caching,
    bookmarks_cache_ttl: Option<Duration>,
    redaction: Redaction,
    scuba_censored_table: Option<String>,
    filestore_params: Option<FilestoreParams>,
    readonly_storage: ReadOnlyStorage,
    derived_data_config: DerivedDataConfig,
    reponame: String,
) -> impl Future<Item = BlobRepo, Error = Error> {
    sql_factory.and_then(move |sql_factory| {
        let redacted_blobs = match redaction {
            Redaction::Enabled => sql_factory
                .open::<SqlRedactedContentStore>()
                .and_then(move |redacted_store| {
                    let redacted_blobs = redacted_store
                        .get_all_redacted_blobs()
                        .map_err(Error::from)
                        .map(HashMap::from_iter);
                    Some(redacted_blobs)
                })
                .left_future(),
            Redaction::Disabled => Ok(None).into_future().right_future(),
        }
        .boxify();

        let filestore_config = filestore_params
            .map(|params| {
                let FilestoreParams {
                    chunk_size,
                    concurrency,
                } = params;

                FilestoreConfig {
                    chunk_size: Some(chunk_size),
                    concurrency,
                }
            })
            .unwrap_or(FilestoreConfig::default());

        match caching {
            Caching::Disabled | Caching::CachelibOnlyBlobstore => {
                let blobstore = if caching == Caching::CachelibOnlyBlobstore {
                    // Use cachelib
                    let blob_pool = try_boxfuture!(get_cache_pool(BLOBSTORE_BLOBS_CACHE_POOL));
                    let presence_pool =
                        try_boxfuture!(get_cache_pool(BLOBSTORE_PRESENCE_CACHE_POOL));

                    unredacted_blobstore
                        .map(move |s| {
                            let s: Arc<dyn Blobstore> = Arc::new(new_cachelib_blobstore_no_lease(
                                s,
                                Arc::new(blob_pool),
                                Arc::new(presence_pool),
                            ));
                            s
                        })
                        .boxify()
                } else {
                    unredacted_blobstore
                };

                new_development(
                    fb,
                    &sql_factory,
                    blobstore,
                    redacted_blobs,
                    scuba_censored_table,
                    repoid,
                    filestore_config,
                    bookmarks_cache_ttl,
                    derived_data_config,
                    reponame,
                )
            }
            Caching::Enabled => new_production(
                fb,
                &sql_factory,
                unredacted_blobstore,
                redacted_blobs,
                scuba_censored_table,
                repoid,
                bookmarks_cache_ttl,
                filestore_config,
                readonly_storage,
                derived_data_config,
                reponame,
            ),
        }
    })
}

/// A helper to build test repositories.
pub struct TestRepoBuilder {
    repo_id: RepositoryId,
    blobstore: Arc<dyn Blobstore>,
    redacted: Option<HashMap<String, String>>,
}

impl TestRepoBuilder {
    pub fn new() -> Self {
        Self {
            repo_id: RepositoryId::new(0),
            blobstore: Arc::new(EagerMemblob::new()),
            redacted: None,
        }
    }

    pub fn id(mut self, repo_id: RepositoryId) -> Self {
        self.repo_id = repo_id;
        self
    }

    pub fn redacted(mut self, redacted: Option<HashMap<String, String>>) -> Self {
        self.redacted = redacted;
        self
    }

    pub fn blobstore(mut self, blobstore: Arc<dyn Blobstore>) -> Self {
        self.blobstore = blobstore;
        self
    }

    fn maybe_blobstore(self, maybe_blobstore: Option<Arc<dyn Blobstore>>) -> Self {
        if let Some(blobstore) = maybe_blobstore {
            return self.blobstore(blobstore);
        }
        self
    }

    pub fn build(self) -> Result<BlobRepo> {
        let Self {
            repo_id,
            blobstore,
            redacted,
        } = self;

        let repo_blobstore_args = RepoBlobstoreArgs::new(
            blobstore,
            redacted,
            repo_id,
            ScubaSampleBuilder::with_discard(),
        );

        let phases_factory = SqlPhasesFactory::with_sqlite_in_memory()?;

        Ok(BlobRepo::new(
            Arc::new(SqlBookmarks::with_sqlite_in_memory()?),
            repo_blobstore_args,
            Arc::new(
                NewFilenodesBuilder::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::Filenodes))?
                    .build(),
            ),
            Arc::new(
                SqlChangesets::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::Changesets))?,
            ),
            Arc::new(
                SqlBonsaiGitMappingConnection::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiGitMapping))?
                    .with_repo_id(repo_id),
            ),
            Arc::new(
                SqlBonsaiGlobalrevMapping::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiGlobalrevMapping))?,
            ),
            Arc::new(
                SqlBonsaiHgMapping::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiHgMapping))?,
            ),
            Arc::new(InProcessLease::new()),
            FilestoreConfig::default(),
            phases_factory,
            init_all_derived_data(),
            "testrepo".to_string(),
        ))
    }
}

/// Used by tests
pub fn new_memblob_empty(blobstore: Option<Arc<dyn Blobstore>>) -> Result<BlobRepo> {
    TestRepoBuilder::new().maybe_blobstore(blobstore).build()
}

/// Used by cross-repo syncing tests
pub fn new_memblob_empty_with_id(
    blobstore: Option<Arc<dyn Blobstore>>,
    repo_id: RepositoryId,
) -> Result<BlobRepo> {
    TestRepoBuilder::new()
        .maybe_blobstore(blobstore)
        .id(repo_id)
        .build()
}

pub fn init_all_derived_data() -> DerivedDataConfig {
    DerivedDataConfig {
        scuba_table: None,
        derived_data_types: btreeset! {
            BlameRoot::NAME.to_string(),
            FilenodesOnlyPublic::NAME.to_string(),
            ChangesetInfo::NAME.to_string(),
            RootFastlog::NAME.to_string(),
            RootFsnodeId::NAME.to_string(),
            RootDeletedManifestId::NAME.to_string(),
            RootUnodeManifestId::NAME.to_string(),
            TreeHandle::NAME.to_string(),
        },
        unode_version: UnodeVersion::V2,
    }
}

// Creates all db tables except for filenodes on the same sqlite connection
pub fn new_memblob_with_sqlite_connection_with_id(
    con: SqliteConnection,
    repo_id: RepositoryId,
) -> Result<(BlobRepo, Connection)> {
    con.execute_batch(SqlBookmarks::CREATION_QUERY)?;
    con.execute_batch(SqlChangesets::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiGitMappingConnection::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiGlobalrevMapping::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiHgMapping::CREATION_QUERY)?;
    con.execute_batch(SqlPhasesFactory::CREATION_QUERY)?;
    let con = Connection::with_sqlite(con);

    new_memblob_with_connection_with_id(con.clone(), repo_id)
}

pub fn new_memblob_with_connection_with_id(
    con: Connection,
    repo_id: RepositoryId,
) -> Result<(BlobRepo, Connection)> {
    let repo_blobstore_args = RepoBlobstoreArgs::new(
        Arc::new(EagerMemblob::new()),
        None,
        repo_id,
        ScubaSampleBuilder::with_discard(),
    );

    let sql_connections = SqlConnections::new_single(con.clone());

    let phases_factory = SqlPhasesFactory::from_sql_connections(sql_connections.clone());

    Ok((
        BlobRepo::new(
            Arc::new(SqlBookmarks::from_sql_connections(sql_connections.clone())),
            repo_blobstore_args,
            // Filenodes are intentionally created on another connection
            Arc::new(
                NewFilenodesBuilder::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::Filenodes))?
                    .build(),
            ),
            Arc::new(SqlChangesets::from_sql_connections(sql_connections.clone())),
            Arc::new(
                SqlBonsaiGitMappingConnection::from_sql_connections(sql_connections.clone())
                    .with_repo_id(repo_id),
            ),
            Arc::new(SqlBonsaiGlobalrevMapping::from_sql_connections(
                sql_connections.clone(),
            )),
            Arc::new(SqlBonsaiHgMapping::from_sql_connections(sql_connections)),
            Arc::new(InProcessLease::new()),
            FilestoreConfig::default(),
            phases_factory,
            init_all_derived_data(),
            "testrepo".to_string(),
        ),
        con,
    ))
}

fn new_development(
    fb: FacebookInit,
    sql_factory: &MetadataSqlFactory,
    unredacted_blobstore: BoxFuture<Arc<dyn Blobstore>, Error>,
    redacted_blobs: BoxFuture<Option<HashMap<String, String>>, Error>,
    scuba_censored_table: Option<String>,
    repoid: RepositoryId,
    filestore_config: FilestoreConfig,
    bookmarks_cache_ttl: Option<Duration>,
    derived_data_config: DerivedDataConfig,
    reponame: String,
) -> BoxFuture<BlobRepo, Error> {
    let bookmarks = sql_factory
        .open::<SqlBookmarks>()
        .context(ErrorKind::StateOpen(StateOpenError::Bookmarks))
        .from_err()
        .map(move |bookmarks| {
            let bookmarks: Arc<dyn Bookmarks> = if let Some(ttl) = bookmarks_cache_ttl {
                Arc::new(CachedBookmarks::new(Arc::new(bookmarks), ttl))
            } else {
                Arc::new(bookmarks)
            };

            bookmarks
        });

    let filenodes_builder = sql_factory
        .open_shardable::<NewFilenodesBuilder>()
        .context(ErrorKind::StateOpen(StateOpenError::Filenodes))
        .from_err();

    let changesets = sql_factory
        .open::<SqlChangesets>()
        .context(ErrorKind::StateOpen(StateOpenError::Changesets))
        .from_err()
        .map(Arc::new);

    let bonsai_git_mapping = {
        cloned!(repoid);
        sql_factory
            .open::<SqlBonsaiGitMappingConnection>()
            .context(ErrorKind::StateOpen(StateOpenError::BonsaiGitMapping))
            .from_err()
            .map(move |conn| Arc::new(conn.with_repo_id(repoid)))
    };

    let bonsai_globalrev_mapping = sql_factory
        .open::<SqlBonsaiGlobalrevMapping>()
        .context(ErrorKind::StateOpen(StateOpenError::BonsaiGlobalrevMapping))
        .from_err()
        .map(Arc::new);

    let bonsai_hg_mapping = sql_factory
        .open::<SqlBonsaiHgMapping>()
        .context(ErrorKind::StateOpen(StateOpenError::BonsaiHgMapping))
        .from_err()
        .map(Arc::new);

    let phases_factory = sql_factory
        .open::<SqlPhasesFactory>()
        .context(ErrorKind::StateOpen(StateOpenError::Phases))
        .from_err();

    bookmarks
        .join5(
            unredacted_blobstore,
            redacted_blobs,
            phases_factory,
            bonsai_git_mapping,
        )
        .join5(
            filenodes_builder,
            changesets,
            bonsai_globalrev_mapping,
            bonsai_hg_mapping,
        )
        .map({
            move |(
                (bookmarks, blobstore, redacted_blobs, phases_factory, bonsai_git_mapping),
                filenodes_builder,
                changesets,
                bonsai_globalrev_mapping,
                bonsai_hg_mapping,
            )| {
                let scuba_builder = ScubaSampleBuilder::with_opt_table(fb, scuba_censored_table);

                BlobRepo::new(
                    bookmarks,
                    RepoBlobstoreArgs::new(blobstore, redacted_blobs, repoid, scuba_builder),
                    Arc::new(filenodes_builder.build()),
                    changesets,
                    bonsai_git_mapping,
                    bonsai_globalrev_mapping,
                    bonsai_hg_mapping,
                    Arc::new(InProcessLease::new()),
                    filestore_config,
                    phases_factory,
                    derived_data_config,
                    reponame,
                )
            }
        })
        .boxify()
}

/// If the DB is remote then set up for a full production configuration.
/// In theory this could be with a local blobstore, but that would just be weird.
fn new_production(
    fb: FacebookInit,
    sql_factory: &MetadataSqlFactory,
    blobstore: BoxFuture<Arc<dyn Blobstore>, Error>,
    redacted_blobs: BoxFuture<Option<HashMap<String, String>>, Error>,
    scuba_censored_table: Option<String>,
    repoid: RepositoryId,
    bookmarks_cache_ttl: Option<Duration>,
    filestore_config: FilestoreConfig,
    readonly_storage: ReadOnlyStorage,
    derived_data_config: DerivedDataConfig,
    reponame: String,
) -> BoxFuture<BlobRepo, Error> {
    fn get_volatile_pool(name: &str) -> Result<cachelib::VolatileLruCachePool> {
        let err = Error::from(ErrorKind::MissingCachePool(name.to_string()));
        cachelib::get_volatile_pool(name)?.ok_or(err)
    }

    let blob_pool = try_boxfuture!(get_cache_pool(BLOBSTORE_BLOBS_CACHE_POOL));
    let presence_pool = try_boxfuture!(get_cache_pool(BLOBSTORE_PRESENCE_CACHE_POOL));

    let blobstore = blobstore
        .and_then(move |blobstore| new_memcache_blobstore(fb, blobstore, "multiplexed", ""));
    let blobstore = blobstore.map(|blobstore| {
        Arc::new(new_cachelib_blobstore_no_lease(
            blobstore,
            Arc::new(blob_pool),
            Arc::new(presence_pool),
        )) as Arc<dyn Blobstore>
    });

    let filenodes_pool = try_boxfuture!(get_volatile_pool("filenodes"));
    let filenodes_history_pool = try_boxfuture!(get_volatile_pool("filenodes_history"));
    let changesets_cache_pool = try_boxfuture!(get_volatile_pool("changesets"));
    let bonsai_hg_mapping_cache_pool = try_boxfuture!(get_volatile_pool("bonsai_hg_mapping"));
    let phases_cache_pool = try_boxfuture!(get_volatile_pool("phases"));

    let derive_data_lease = try_boxfuture!(MemcacheOps::new(fb, "derived-data-lease", ""));

    let filenodes_tier = sql_factory
        .tier_name_shardable::<NewFilenodesBuilder>()
        .into_future();
    let filenodes_builder = sql_factory.open_shardable::<NewFilenodesBuilder>();
    let filenodes_tier_and_builder = filenodes_tier.join(filenodes_builder);
    let bookmarks = sql_factory.open::<SqlBookmarks>().map(Arc::new);
    let changesets = sql_factory.open::<SqlChangesets>().map(Arc::new);
    let bonsai_git_mapping = {
        cloned!(repoid);
        sql_factory
            .open::<SqlBonsaiGitMappingConnection>()
            .map(move |conn| Arc::new(conn.with_repo_id(repoid)))
    };
    let bonsai_globalrev_mapping = sql_factory
        .open::<SqlBonsaiGlobalrevMapping>()
        .map(Arc::new);
    let bonsai_hg_mapping = sql_factory.open::<SqlBonsaiHgMapping>().map(Arc::new);
    let phases_factory = sql_factory.open::<SqlPhasesFactory>();

    // Wrap again to avoid any writes to memcache
    let blobstore = if readonly_storage.0 {
        blobstore
            .map(|inner| Arc::new(ReadOnlyBlobstore::new(inner)) as Arc<dyn Blobstore>)
            .left_future()
    } else {
        blobstore.right_future()
    };

    filenodes_tier_and_builder
        .join5(
            blobstore,
            redacted_blobs,
            phases_factory,
            bonsai_git_mapping,
        )
        .join5(
            bookmarks,
            changesets,
            bonsai_globalrev_mapping,
            bonsai_hg_mapping,
        )
        .map(
            move |(
                (
                    (filenodes_tier, mut filenodes_builder),
                    blobstore,
                    redacted_blobs,
                    mut phases_factory,
                    bonsai_git_mapping,
                ),
                bookmarks,
                changesets,
                bonsai_globalrev_mapping,
                bonsai_hg_mapping,
            )| {
                filenodes_builder.enable_caching(
                    fb,
                    filenodes_pool,
                    filenodes_history_pool,
                    "newfilenodes",
                    &filenodes_tier,
                );

                let bookmarks: Arc<dyn Bookmarks> = {
                    if let Some(ttl) = bookmarks_cache_ttl {
                        Arc::new(CachedBookmarks::new(bookmarks, ttl))
                    } else {
                        bookmarks
                    }
                };

                let changesets = Arc::new(CachingChangesets::new(
                    fb,
                    changesets,
                    changesets_cache_pool,
                ));

                let bonsai_hg_mapping = CachingBonsaiHgMapping::new(
                    fb,
                    bonsai_hg_mapping,
                    bonsai_hg_mapping_cache_pool,
                );

                phases_factory.enable_caching(fb, phases_cache_pool);
                let scuba_builder = ScubaSampleBuilder::with_opt_table(fb, scuba_censored_table);

                BlobRepo::new(
                    bookmarks,
                    RepoBlobstoreArgs::new(blobstore, redacted_blobs, repoid, scuba_builder),
                    Arc::new(filenodes_builder.build()) as Arc<dyn Filenodes>,
                    changesets,
                    bonsai_git_mapping,
                    bonsai_globalrev_mapping,
                    Arc::new(bonsai_hg_mapping),
                    Arc::new(derive_data_lease),
                    filestore_config,
                    phases_factory,
                    derived_data_config,
                    reponame,
                )
            },
        )
        .boxify()
}

fn get_cache_pool(name: &str) -> Result<cachelib::LruCachePool> {
    let err = Error::from(ErrorKind::MissingCachePool(name.to_string()));
    cachelib::get_pool(name).ok_or(err)
}
