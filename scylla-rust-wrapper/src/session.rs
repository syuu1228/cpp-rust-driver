use crate::argconv::*;
use crate::batch::CassBatch;
use crate::cass_error::*;
use crate::cass_types::{
    get_column_type, get_column_type_from_cql_type, CassDataType, CassDataTypeArc, UDTDataType,
};
use crate::cluster::build_session_builder;
use crate::cluster::CassCluster;
use crate::future::{CassFuture, CassResultValue};
use crate::query_result::Value::{CollectionValue, RegularValue};
use crate::query_result::{
    CassResult, CassResultData, CassResult_, CassRow, CassValue, Collection, Value,
};
use crate::statement::CassStatement;
use crate::statement::Statement;
use crate::types::{cass_uint64_t, size_t};
use scylla::frame::response::result::{CqlValue, Row};
use scylla::frame::types::Consistency;
use scylla::query::Query;
use scylla::transport::errors::QueryError;
use scylla::transport::topology::ColumnKind;
use scylla::{QueryResult, Session};
use std::collections::HashMap;
use std::future::Future;
use std::os::raw::c_char;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

include!(concat!(env!("OUT_DIR"), "/cppdriver_column_type.rs"));

pub type CassSession = RwLock<Option<Session>>;
type CassSession_ = Arc<CassSession>;

pub type CassKeyspaceMeta_ = &'static CassKeyspaceMeta;

pub struct CassKeyspaceMeta {
    name: String,

    // User defined type name to type
    pub user_defined_type_data_type: HashMap<String, Arc<CassDataType>>,
    pub tables: HashMap<String, CassTableMeta>,
}

pub type CassTableMeta_ = &'static CassTableMeta;

pub struct CassTableMeta {
    pub name: String,
    pub columns_metadata: HashMap<String, CassColumnMeta>,
    pub partition_keys: Vec<String>,
    pub clustering_keys: Vec<String>,
}

pub struct CassColumnMeta {
    pub name: String,
    pub column_type: CassDataType,
    pub column_kind: CassColumnType,
}

pub type CassSchemaMeta_ = &'static CassSchemaMeta;

pub struct CassSchemaMeta {
    pub keyspaces: HashMap<String, CassKeyspaceMeta>,
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_new() -> *const CassSession {
    let session: CassSession_ = Arc::new(RwLock::new(None));
    Arc::into_raw(session)
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_connect(
    session_raw: *mut CassSession,
    cluster_raw: *const CassCluster,
) -> *const CassFuture {
    let session_opt = ptr_to_ref(session_raw);
    let cluster: CassCluster = (*ptr_to_ref(cluster_raw)).clone();

    CassFuture::make_raw(async move {
        // This can sleep for a long time, but only if someone connects/closes session
        // from more than 1 thread concurrently, which is inherently stupid thing to do.
        let mut session_guard = session_opt.write().await;
        if session_guard.is_some() {
            return Err((
                CassError::CASS_ERROR_LIB_UNABLE_TO_CONNECT,
                "Already connecting, closing, or connected".msg(),
            ));
        }

        let session = build_session_builder(&cluster)
            .build()
            .await
            .map_err(|err| (CassError::from(&err), err.msg()))?;

        *session_guard = Some(session);
        Ok(CassResultValue::Empty)
    })
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_execute_batch(
    session_raw: *mut CassSession,
    batch_raw: *const CassBatch,
) -> *const CassFuture {
    let session_opt = ptr_to_ref(session_raw);
    let batch_from_raw = ptr_to_ref(batch_raw);
    let state = batch_from_raw.state.clone();
    let request_timeout_ms = batch_from_raw.batch_request_timeout_ms;

    let future = async move {
        let session_guard = session_opt.read().await;
        if session_guard.is_none() {
            return Err((
                CassError::CASS_ERROR_LIB_NO_HOSTS_AVAILABLE,
                "Session is not connected".msg(),
            ));
        }
        let session = session_guard.as_ref().unwrap();

        let query_res = session.batch(&state.batch, &state.bound_values).await;
        match query_res {
            Ok(_result) => Ok(CassResultValue::QueryResult(Arc::new(CassResult {
                rows: None,
                metadata: Arc::new(CassResultData {
                    paging_state: None,
                    col_specs: vec![],
                    tracing_id: None,
                }),
            }))),
            Err(err) => Ok(CassResultValue::QueryError(Arc::new(err))),
        }
    };

    match request_timeout_ms {
        Some(timeout_ms) => {
            CassFuture::make_raw(async move { request_with_timeout(timeout_ms, future).await })
        }
        None => CassFuture::make_raw(future),
    }
}

async fn request_with_timeout(
    request_timeout_ms: cass_uint64_t,
    future: impl Future<Output = Result<CassResultValue, (CassError, String)>>,
) -> Result<CassResultValue, (CassError, String)> {
    match tokio::time::timeout(Duration::from_millis(request_timeout_ms), future).await {
        Ok(result) => result,
        Err(_timeout_err) => Ok(CassResultValue::QueryError(Arc::new(
            QueryError::TimeoutError,
        ))),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_execute(
    session_raw: *mut CassSession,
    statement_raw: *const CassStatement,
) -> *const CassFuture {
    let session_opt = ptr_to_ref(session_raw);
    let statement_opt = ptr_to_ref(statement_raw);
    let paging_state = statement_opt.paging_state.clone();
    let bound_values = statement_opt.bound_values.clone();
    let request_timeout_ms = statement_opt.request_timeout_ms;

    let statement = statement_opt.statement.clone();

    let future = async move {
        let session_guard = session_opt.read().await;
        if session_guard.is_none() {
            return Err((
                CassError::CASS_ERROR_LIB_NO_HOSTS_AVAILABLE,
                "Session is not connected".msg(),
            ));
        }
        let session = session_guard.as_ref().unwrap();

        let query_res: Result<QueryResult, QueryError> = match statement {
            Statement::Simple(query) => {
                session
                    .query_paged(query.query, bound_values, paging_state)
                    .await
            }
            Statement::Prepared(prepared) => {
                session
                    .execute_paged(&prepared, bound_values, paging_state)
                    .await
            }
        };

        match query_res {
            Ok(result) => {
                let metadata = Arc::new(CassResultData {
                    paging_state: result.paging_state,
                    col_specs: result.col_specs,
                    tracing_id: result.tracing_id,
                });
                let cass_rows = create_cass_rows_from_rows(result.rows, &metadata);
                let cass_result: CassResult_ = Arc::new(CassResult {
                    rows: cass_rows,
                    metadata,
                });

                Ok(CassResultValue::QueryResult(cass_result))
            }
            Err(err) => Ok(CassResultValue::QueryError(Arc::new(err))),
        }
    };

    match request_timeout_ms {
        Some(timeout_ms) => {
            CassFuture::make_raw(async move { request_with_timeout(timeout_ms, future).await })
        }
        None => CassFuture::make_raw(future),
    }
}

fn create_cass_rows_from_rows(
    rows: Option<Vec<Row>>,
    metadata: &Arc<CassResultData>,
) -> Option<Vec<CassRow>> {
    let rows = rows?;
    let cass_rows = rows
        .into_iter()
        .map(|r| CassRow {
            columns: create_cass_row_columns(r, metadata),
            result_metadata: metadata.clone(),
        })
        .collect();

    Some(cass_rows)
}

fn create_cass_row_columns(row: Row, metadata: &Arc<CassResultData>) -> Vec<CassValue> {
    row.columns
        .into_iter()
        .zip(metadata.col_specs.iter())
        .map(|(val, col)| {
            let column_type = Arc::new(get_column_type(&col.typ));
            CassValue {
                value: val.map(|col_val| get_column_value(col_val, &column_type)),
                value_type: column_type,
            }
        })
        .collect()
}

fn get_column_value(column: CqlValue, column_type: &CassDataTypeArc) -> Value {
    match (column, column_type.as_ref()) {
        (CqlValue::List(list), CassDataType::List(Some(list_type))) => {
            CollectionValue(Collection::List(
                list.into_iter()
                    .map(|val| CassValue {
                        value_type: list_type.clone(),
                        value: Some(get_column_value(val, list_type)),
                    })
                    .collect(),
            ))
        }
        (CqlValue::Map(map), CassDataType::Map(Some(key_type), Some(value_type))) => {
            CollectionValue(Collection::Map(
                map.into_iter()
                    .map(|(key, val)| {
                        (
                            CassValue {
                                value_type: key_type.clone(),
                                value: Some(get_column_value(key, key_type)),
                            },
                            CassValue {
                                value_type: value_type.clone(),
                                value: Some(get_column_value(val, value_type)),
                            },
                        )
                    })
                    .collect(),
            ))
        }
        (CqlValue::Set(set), CassDataType::Set(Some(set_type))) => {
            CollectionValue(Collection::Set(
                set.into_iter()
                    .map(|val| CassValue {
                        value_type: set_type.clone(),
                        value: Some(get_column_value(val, set_type)),
                    })
                    .collect(),
            ))
        }
        (
            CqlValue::UserDefinedType {
                keyspace,
                type_name,
                fields,
            },
            CassDataType::UDT(udt_type),
        ) => CollectionValue(Collection::UserDefinedType {
            keyspace,
            type_name,
            fields: fields
                .into_iter()
                .enumerate()
                .map(|(index, (name, val_opt))| {
                    let udt_field_type_opt = udt_type.get_field_by_index(index);
                    if let (Some(val), Some(udt_field_type)) = (val_opt, udt_field_type_opt) {
                        return (
                            name,
                            Some(CassValue {
                                value_type: udt_field_type.clone(),
                                value: Some(get_column_value(val, udt_field_type)),
                            }),
                        );
                    }
                    (name, None)
                })
                .collect(),
        }),
        (CqlValue::Tuple(tuple), CassDataType::Tuple(tuple_types)) => {
            CollectionValue(Collection::Tuple(
                tuple
                    .into_iter()
                    .enumerate()
                    .map(|(index, val_opt)| {
                        val_opt
                            .zip(tuple_types.get(index))
                            .map(|(val, tuple_field_type)| CassValue {
                                value_type: tuple_field_type.clone(),
                                value: Some(get_column_value(val, tuple_field_type)),
                            })
                    })
                    .collect(),
            ))
        }
        (regular_value, _) => RegularValue(regular_value),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_prepare_from_existing(
    cass_session: *mut CassSession,
    statement: *const CassStatement,
) -> *const CassFuture {
    let session = ptr_to_ref(cass_session);
    let cass_statement = ptr_to_ref(statement);
    let statement = cass_statement.statement.clone();

    CassFuture::make_raw(async move {
        let query = match &statement {
            Statement::Simple(q) => q,
            Statement::Prepared(ps) => {
                return Ok(CassResultValue::Prepared(ps.clone()));
            }
        };

        let session_guard = session.read().await;
        if session_guard.is_none() {
            return Err((
                CassError::CASS_ERROR_LIB_NO_HOSTS_AVAILABLE,
                "Session is not connected".msg(),
            ));
        }
        let session = session_guard.as_ref().unwrap();
        let prepared = session
            .prepare(query.query.clone())
            .await
            .map_err(|err| (CassError::from(&err), err.msg()))?;

        Ok(CassResultValue::Prepared(Arc::new(prepared)))
    })
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_prepare(
    session: *mut CassSession,
    query: *const c_char,
) -> *const CassFuture {
    cass_session_prepare_n(session, query, strlen(query))
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_prepare_n(
    cass_session_raw: *mut CassSession,
    query: *const c_char,
    query_length: size_t,
) -> *const CassFuture {
    let query_str = match ptr_to_cstr_n(query, query_length) {
        Some(v) => v,
        None => return std::ptr::null(),
    };
    let query = Query::new(query_str.to_string());
    let cass_session: &CassSession = ptr_to_ref(cass_session_raw);

    CassFuture::make_raw(async move {
        let session_guard = cass_session.read().await;
        if session_guard.is_none() {
            return Err((
                CassError::CASS_ERROR_LIB_NO_HOSTS_AVAILABLE,
                "Session is not connected".msg(),
            ));
        }
        let session = session_guard.as_ref().unwrap();

        let mut prepared = session
            .prepare(query)
            .await
            .map_err(|err| (CassError::from(&err), err.msg()))?;

        // Set Cpp Driver default configuration for queries:
        prepared.disable_paging();
        prepared.set_consistency(Consistency::One);

        Ok(CassResultValue::Prepared(Arc::new(prepared)))
    })
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_free(session_raw: *mut CassSession) {
    free_arced(session_raw);
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_close(session: *mut CassSession) -> *const CassFuture {
    let session_opt = ptr_to_ref(session);

    CassFuture::make_raw(async move {
        let mut session_guard = session_opt.write().await;
        if session_guard.is_none() {
            return Err((
                CassError::CASS_ERROR_LIB_UNABLE_TO_CLOSE,
                "Already closing or closed".msg(),
            ));
        }

        *session_guard = None;

        Ok(CassResultValue::Empty)
    })
}

#[no_mangle]
pub unsafe extern "C" fn cass_session_get_schema_meta(
    session: *const CassSession,
) -> *const CassSchemaMeta {
    let cass_session = ptr_to_ref(session);
    let mut keyspaces: HashMap<String, CassKeyspaceMeta> = HashMap::new();

    for (keyspace_name, keyspace) in cass_session
        .blocking_read()
        .as_ref()
        .unwrap()
        .get_cluster_data()
        .get_keyspace_info()
    {
        let mut user_defined_type_data_type = HashMap::new();
        let mut tables = HashMap::new();

        for udt_name in keyspace.user_defined_types.keys() {
            user_defined_type_data_type.insert(
                udt_name.clone(),
                Arc::new(CassDataType::UDT(UDTDataType::create_with_params(
                    &keyspace.user_defined_types,
                    keyspace_name,
                    udt_name,
                ))),
            );
        }

        for (table_name, table_metadata) in &keyspace.tables {
            let columns_metadata: HashMap<String, CassColumnMeta> = table_metadata
                .columns
                .iter()
                .map(|(column_name, column_metadata)| {
                    let cass_column_meta = CassColumnMeta {
                        name: column_name.clone(),
                        column_type: get_column_type_from_cql_type(
                            &column_metadata.type_,
                            &keyspace.user_defined_types,
                            keyspace_name,
                        ),
                        column_kind: match column_metadata.kind {
                            ColumnKind::Regular => CassColumnType::CASS_COLUMN_TYPE_REGULAR,
                            ColumnKind::Static => CassColumnType::CASS_COLUMN_TYPE_STATIC,
                            ColumnKind::Clustering => {
                                CassColumnType::CASS_COLUMN_TYPE_CLUSTERING_KEY
                            }
                            ColumnKind::PartitionKey => {
                                CassColumnType::CASS_COLUMN_TYPE_PARTITION_KEY
                            }
                        },
                    };

                    (column_name.clone(), cass_column_meta)
                })
                .collect();

            let cass_table_meta = CassTableMeta {
                name: table_name.clone(),
                columns_metadata,
                partition_keys: table_metadata.partition_key.clone(),
                clustering_keys: table_metadata.clustering_key.clone(),
            };

            tables.insert(table_name.clone(), cass_table_meta);
        }

        keyspaces.insert(
            keyspace_name.clone(),
            CassKeyspaceMeta {
                name: keyspace_name.clone(),
                user_defined_type_data_type,
                tables,
            },
        );
    }

    Box::into_raw(Box::new(CassSchemaMeta { keyspaces }))
}

#[no_mangle]
pub unsafe extern "C" fn cass_schema_meta_free(schema_meta: *mut CassSchemaMeta) {
    free_boxed(schema_meta)
}

#[no_mangle]
pub unsafe extern "C" fn cass_schema_meta_keyspace_by_name(
    schema_meta: *const CassSchemaMeta,
    keyspace_name: *const c_char,
) -> *const CassKeyspaceMeta {
    cass_schema_meta_keyspace_by_name_n(schema_meta, keyspace_name, strlen(keyspace_name))
}

#[no_mangle]
pub unsafe extern "C" fn cass_schema_meta_keyspace_by_name_n(
    schema_meta: *const CassSchemaMeta,
    keyspace_name: *const c_char,
    keyspace_name_length: size_t,
) -> *const CassKeyspaceMeta {
    if keyspace_name.is_null() {
        return std::ptr::null();
    }

    let metadata = ptr_to_ref(schema_meta);
    let keyspace = ptr_to_cstr_n(keyspace_name, keyspace_name_length).unwrap();

    let keyspace_meta = metadata.keyspaces.get(keyspace);

    match keyspace_meta {
        Some(meta) => meta,
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_keyspace_meta_name(
    keyspace_meta: *const CassKeyspaceMeta,
    name: *mut *const c_char,
    name_length: *mut size_t,
) {
    let keyspace_meta = ptr_to_ref(keyspace_meta);
    write_str_to_c(keyspace_meta.name.as_str(), name, name_length)
}

#[no_mangle]
pub unsafe extern "C" fn cass_keyspace_meta_user_type_by_name(
    keyspace_meta: *const CassKeyspaceMeta,
    type_: *const c_char,
) -> *const CassDataType {
    cass_keyspace_meta_user_type_by_name_n(keyspace_meta, type_, strlen(type_))
}

#[no_mangle]
pub unsafe extern "C" fn cass_keyspace_meta_user_type_by_name_n(
    keyspace_meta: *const CassKeyspaceMeta,
    type_: *const c_char,
    type_length: size_t,
) -> *const CassDataType {
    if type_.is_null() {
        return std::ptr::null();
    }

    let keyspace_meta = ptr_to_ref(keyspace_meta);
    let user_type_name = ptr_to_cstr_n(type_, type_length).unwrap();

    match keyspace_meta
        .user_defined_type_data_type
        .get(user_type_name)
    {
        Some(udt) => Arc::into_raw(udt.clone()) as *const CassDataType,
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_keyspace_meta_table_by_name(
    keyspace_meta: *const CassKeyspaceMeta,
    table: *const c_char,
) -> *const CassTableMeta {
    cass_keyspace_meta_table_by_name_n(keyspace_meta, table, strlen(table))
}

#[no_mangle]
pub unsafe extern "C" fn cass_keyspace_meta_table_by_name_n(
    keyspace_meta: *const CassKeyspaceMeta,
    table: *const c_char,
    table_length: size_t,
) -> *const CassTableMeta {
    if table.is_null() {
        return std::ptr::null();
    }

    let keyspace_meta = ptr_to_ref(keyspace_meta);
    let table_name = ptr_to_cstr_n(table, table_length).unwrap();

    let table_meta = keyspace_meta.tables.get(table_name);

    match table_meta {
        Some(meta) => meta as *const CassTableMeta,
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_name(
    table_meta: *const CassTableMeta,
    name: *mut *const c_char,
    name_length: *mut size_t,
) {
    let table_meta = ptr_to_ref(table_meta);
    write_str_to_c(table_meta.name.as_str(), name, name_length)
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_column_count(table_meta: *const CassTableMeta) -> size_t {
    let table_meta = ptr_to_ref(table_meta);
    table_meta.columns_metadata.len() as size_t
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_partition_key(
    table_meta: *const CassTableMeta,
    index: size_t,
) -> *const CassColumnMeta {
    let table_meta = ptr_to_ref(table_meta);

    match table_meta.partition_keys.get(index as usize) {
        Some(column_name) => match table_meta.columns_metadata.get(column_name) {
            Some(column_meta) => column_meta as *const CassColumnMeta,
            None => std::ptr::null(),
        },
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_partition_key_count(
    table_meta: *const CassTableMeta,
) -> size_t {
    let table_meta = ptr_to_ref(table_meta);
    table_meta.partition_keys.len() as size_t
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_clustering_key(
    table_meta: *const CassTableMeta,
    index: size_t,
) -> *const CassColumnMeta {
    let table_meta = ptr_to_ref(table_meta);

    match table_meta.clustering_keys.get(index as usize) {
        Some(column_name) => match table_meta.columns_metadata.get(column_name) {
            Some(column_meta) => column_meta as *const CassColumnMeta,
            None => std::ptr::null(),
        },
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_clustering_key_count(
    table_meta: *const CassTableMeta,
) -> size_t {
    let table_meta = ptr_to_ref(table_meta);
    table_meta.clustering_keys.len() as size_t
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_column_by_name(
    table_meta: *const CassTableMeta,
    column: *const c_char,
) -> *const CassColumnMeta {
    cass_table_meta_column_by_name_n(table_meta, column, strlen(column))
}

#[no_mangle]
pub unsafe extern "C" fn cass_table_meta_column_by_name_n(
    table_meta: *const CassTableMeta,
    column: *const c_char,
    column_length: size_t,
) -> *const CassColumnMeta {
    if column.is_null() {
        return std::ptr::null();
    }

    let table_meta = ptr_to_ref(table_meta);
    let column_name = ptr_to_cstr_n(column, column_length).unwrap();

    match table_meta.columns_metadata.get(column_name) {
        Some(column_meta) => column_meta as *const CassColumnMeta,
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn cass_column_meta_name(
    column_meta: *const CassColumnMeta,
    name: *mut *const c_char,
    name_length: *mut size_t,
) {
    let column_meta = ptr_to_ref(column_meta);
    write_str_to_c(column_meta.name.as_str(), name, name_length)
}

#[no_mangle]
pub unsafe extern "C" fn cass_column_meta_data_type(
    column_meta: *const CassColumnMeta,
) -> *const CassDataType {
    let column_meta = ptr_to_ref(column_meta);
    &column_meta.column_type as *const CassDataType
}

#[no_mangle]
pub unsafe extern "C" fn cass_column_meta_type(
    column_meta: *const CassColumnMeta,
) -> CassColumnType {
    let column_meta = ptr_to_ref(column_meta);
    column_meta.column_kind
}
