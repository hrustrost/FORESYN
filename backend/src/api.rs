use std::sync::Arc;

use alloy::primitives::U256;
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tower_http::cors::CorsLayer;
use tracing::error;

use crate::{
    metadata::MarketMetadata,
    read_repository::{MarketReadModel, MarketReader, PositionReadModel, ReadError},
};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

#[derive(Clone)]
pub struct ApiState {
    reader: Arc<dyn MarketReader>,
}

impl ApiState {
    pub fn new(reader: Arc<dyn MarketReader>) -> Self {
        Self { reader }
    }
}

#[derive(Debug, Error)]
pub enum ApiRouterError {
    #[error("configured CORS origin is not a valid HTTP header value")]
    InvalidCorsOrigin(#[from] axum::http::header::InvalidHeaderValue),
}

pub fn router(state: ApiState, cors_origin: &str) -> Result<Router, ApiRouterError> {
    let cors = CorsLayer::new()
        .allow_origin(HeaderValue::from_str(cors_origin)?)
        .allow_methods([Method::GET])
        .allow_headers([header::CONTENT_TYPE]);

    Ok(Router::new()
        .route("/health", get(health))
        .route("/api/markets", get(list_markets))
        .route("/api/markets/{market_id}", get(market))
        .route("/api/markets/{market_id}/positions", get(market_positions))
        .with_state(state)
        .layer(cors))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MarketResponse {
    pub market_id: String,
    pub resolver: String,
    pub creator: String,
    pub deadline: String,
    pub metadata_digest: String,
    pub creation_block_number: String,
    pub yes_pool: String,
    pub no_pool: String,
    pub total_pool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MarketMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_verified: Option<bool>,
}

impl From<MarketReadModel> for MarketResponse {
    fn from(value: MarketReadModel) -> Self {
        Self {
            market_id: value.market_id,
            resolver: value.resolver,
            creator: value.creator,
            deadline: value.deadline,
            metadata_digest: value.metadata_digest,
            creation_block_number: value.creation_block_number,
            yes_pool: value.yes_pool,
            no_pool: value.no_pool,
            total_pool: value.total_pool,
            metadata: value.metadata,
            metadata_verified: value.metadata_verified,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PositionResponse {
    pub user_address: String,
    pub yes_stake: String,
    pub no_stake: String,
    pub total_stake: String,
    pub updated_block_number: String,
}

impl From<PositionReadModel> for PositionResponse {
    fn from(value: PositionReadModel) -> Self {
        Self {
            user_address: value.user_address,
            yes_stake: value.yes_stake,
            no_stake: value.no_stake,
            total_stake: value.total_stake,
            updated_block_number: value.updated_block_number,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Pagination {
    limit: Option<u32>,
    offset: Option<u64>,
}

impl Pagination {
    fn normalized(self) -> (i64, i64) {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = self.offset.unwrap_or(0).min(i64::MAX as u64);
        (i64::from(limit), offset as i64)
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[derive(Debug)]
enum ApiError {
    MarketNotFound,
    InvalidMarketId,
    InvalidPagination,
    Internal(ReadError),
}

impl From<ReadError> for ApiError {
    fn from(value: ReadError) -> Self {
        Self::Internal(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_code) = match self {
            Self::MarketNotFound => (StatusCode::NOT_FOUND, "market_not_found"),
            Self::InvalidMarketId => (StatusCode::BAD_REQUEST, "invalid_market_id"),
            Self::InvalidPagination => (StatusCode::BAD_REQUEST, "invalid_pagination"),
            Self::Internal(source) => {
                error!(error = %source, "REST read failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        (status, Json(ErrorResponse { error: error_code })).into_response()
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn list_markets(
    State(state): State<ApiState>,
    pagination: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<Vec<MarketResponse>>, ApiError> {
    let pagination = pagination.map_err(|_| ApiError::InvalidPagination)?.0;
    let (limit, offset) = pagination.normalized();
    let markets = state.reader.list_markets(limit, offset).await?;
    Ok(Json(markets.into_iter().map(Into::into).collect()))
}

async fn market(
    State(state): State<ApiState>,
    Path(market_id): Path<String>,
) -> Result<Json<MarketResponse>, ApiError> {
    let market_id = canonical_market_id(&market_id)?;
    let market = state
        .reader
        .market(&market_id)
        .await?
        .ok_or(ApiError::MarketNotFound)?;
    Ok(Json(market.into()))
}

async fn market_positions(
    State(state): State<ApiState>,
    Path(market_id): Path<String>,
) -> Result<Json<Vec<PositionResponse>>, ApiError> {
    let market_id = canonical_market_id(&market_id)?;
    if state.reader.market(&market_id).await?.is_none() {
        return Err(ApiError::MarketNotFound);
    }
    let positions = state.reader.positions(&market_id).await?;
    Ok(Json(positions.into_iter().map(Into::into).collect()))
}

fn canonical_market_id(value: &str) -> Result<String, ApiError> {
    value
        .parse::<U256>()
        .map(|value| value.to_string())
        .map_err(|_| ApiError::InvalidMarketId)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{HeaderValue, Request, StatusCode, header},
    };
    use serde_json::{Value, json};
    use sqlx::{PgPool, error::ErrorKind, postgres::PgPoolOptions};
    use tower::ServiceExt;

    use super::{ApiState, router};
    use crate::{
        db::{Database, POSTGRES_TEST_LOCK},
        read_repository::{
            MarketReadModel, MarketReader, PositionReadModel, PostgresMarketReader, ReadError,
        },
    };

    #[derive(Default)]
    struct FakeReader {
        markets: Mutex<Vec<MarketReadModel>>,
        positions: Mutex<Vec<PositionReadModel>>,
        list_calls: Mutex<Vec<(i64, i64)>>,
        fail: bool,
    }

    #[async_trait]
    impl MarketReader for FakeReader {
        async fn list_markets(
            &self,
            limit: i64,
            offset: i64,
        ) -> Result<Vec<MarketReadModel>, ReadError> {
            self.list_calls.lock().unwrap().push((limit, offset));
            if self.fail {
                return Err(sensitive_database_error());
            }
            Ok(self.markets.lock().unwrap().clone())
        }

        async fn market(&self, market_id: &str) -> Result<Option<MarketReadModel>, ReadError> {
            if self.fail {
                return Err(sensitive_database_error());
            }
            Ok(self
                .markets
                .lock()
                .unwrap()
                .iter()
                .find(|market| market.market_id == market_id)
                .cloned())
        }

        async fn positions(&self, _market_id: &str) -> Result<Vec<PositionReadModel>, ReadError> {
            if self.fail {
                return Err(sensitive_database_error());
            }
            Ok(self.positions.lock().unwrap().clone())
        }
    }

    fn sensitive_database_error() -> ReadError {
        ReadError::Sql(sqlx::Error::Database(Box::new(FakeDatabaseError)))
    }

    #[derive(Debug)]
    struct FakeDatabaseError;

    impl std::fmt::Display for FakeDatabaseError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("postgres://admin:secret@private-host/production")
        }
    }

    impl std::error::Error for FakeDatabaseError {}

    impl sqlx::error::DatabaseError for FakeDatabaseError {
        fn message(&self) -> &str {
            "postgres://admin:secret@private-host/production"
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn market_model() -> MarketReadModel {
        MarketReadModel {
            market_id: "1".into(),
            resolver: "0x1111111111111111111111111111111111111111".into(),
            creator: "0x2222222222222222222222222222222222222222".into(),
            deadline: "18446744073709551615".into(),
            metadata_digest: "0x3333333333333333333333333333333333333333333333333333333333333333"
                .into(),
            creation_block_number: "100".into(),
            yes_pool: "340282366920938463463374607431768211455".into(),
            no_pool: "5".into(),
            total_pool: "340282366920938463463374607431768211460".into(),
        }
    }

    fn position_model() -> PositionReadModel {
        PositionReadModel {
            user_address: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            yes_stake: "3".into(),
            no_stake: "7".into(),
            total_stake: "10".into(),
            updated_block_number: "103".into(),
        }
    }

    fn app(reader: Arc<dyn MarketReader>) -> axum::Router {
        router(ApiState::new(reader), "http://localhost:5173").unwrap()
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn empty_list_and_pagination_defaults_and_bounds_are_stable() {
        let reader = Arc::new(FakeReader::default());
        let application = app(reader.clone());

        let empty = application
            .clone()
            .oneshot(Request::get("/api/markets").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(response_json(empty).await, json!([]));

        let bounded = application
            .oneshot(
                Request::get("/api/markets?limit=999&offset=12")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bounded.status(), StatusCode::OK);
        assert_eq!(*reader.list_calls.lock().unwrap(), vec![(20, 0), (100, 12)]);
    }

    #[tokio::test]
    async fn market_and_positions_serialize_exact_strings_and_hex() {
        let reader = Arc::new(FakeReader {
            markets: Mutex::new(vec![market_model()]),
            positions: Mutex::new(vec![position_model()]),
            ..FakeReader::default()
        });
        let application = app(reader);

        let market = application
            .clone()
            .oneshot(Request::get("/api/markets/1").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(market.status(), StatusCode::OK);
        assert_eq!(
            response_json(market).await,
            json!({
                "market_id": "1",
                "resolver": "0x1111111111111111111111111111111111111111",
                "creator": "0x2222222222222222222222222222222222222222",
                "deadline": "18446744073709551615",
                "metadata_digest": "0x3333333333333333333333333333333333333333333333333333333333333333",
                "creation_block_number": "100",
                "yes_pool": "340282366920938463463374607431768211455",
                "no_pool": "5",
                "total_pool": "340282366920938463463374607431768211460"
            })
        );

        let positions = application
            .oneshot(
                Request::get("/api/markets/1/positions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(positions.status(), StatusCode::OK);
        assert_eq!(
            response_json(positions).await,
            json!([{
                "user_address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "yes_stake": "3",
                "no_stake": "7",
                "total_stake": "10",
                "updated_block_number": "103"
            }])
        );
    }

    #[tokio::test]
    async fn missing_market_is_404_for_detail_and_positions() {
        let application = app(Arc::new(FakeReader::default()));
        for path in ["/api/markets/9", "/api/markets/9/positions"] {
            let response = application
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                response_json(response).await,
                json!({"error": "market_not_found"})
            );
        }
    }

    #[tokio::test]
    async fn database_errors_return_stable_json_without_sensitive_details() {
        let application = app(Arc::new(FakeReader {
            fail: true,
            ..FakeReader::default()
        }));

        let response = application
            .oneshot(Request::get("/api/markets").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_json(response).await;
        assert_eq!(body, json!({"error": "internal_error"}));
        assert!(!body.to_string().contains("secret"));
        assert!(!body.to_string().contains("private-host"));
    }

    #[tokio::test]
    async fn health_and_cors_remain_available() {
        let response = app(Arc::new(FakeReader::default()))
            .oneshot(
                Request::get("/health")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://localhost:5173"))
        );
    }

    async fn integration_pool() -> Option<PgPool> {
        let database_url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(
            PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn postgres_api_is_scoped_and_preserves_exact_projection_values() {
        let Some(pool) = integration_pool().await else {
            eprintln!("skipping PostgreSQL API test: TEST_DATABASE_URL is not set");
            return;
        };
        let _guard = POSTGRES_TEST_LOCK.lock().await;
        Database::from_pool(pool.clone()).migrate().await.unwrap();

        let chain_id = 71_337_i64;
        let other_chain_id = 71_338_i64;
        let contract = [0x44_u8; 20];
        let other_contract = [0x55_u8; 20];
        sqlx::query("DELETE FROM indexed_blocks WHERE chain_id IN ($1, $2)")
            .bind(chain_id)
            .bind(other_chain_id)
            .execute(&pool)
            .await
            .unwrap();
        for (chain, hash_byte) in [(chain_id, 0x10_u8), (other_chain_id, 0x20_u8)] {
            sqlx::query(
                "INSERT INTO indexed_blocks
                    (chain_id, block_number, block_hash, parent_hash, block_timestamp)
                 VALUES ($1, 100, $2, $3, now())",
            )
            .bind(chain)
            .bind(vec![hash_byte; 32])
            .bind(vec![hash_byte.saturating_sub(1); 32])
            .execute(&pool)
            .await
            .unwrap();
        }

        async fn insert_market(
            pool: &PgPool,
            chain_id: i64,
            contract: &[u8],
            market_id: &str,
            resolver_byte: u8,
            creator_byte: u8,
            digest_byte: u8,
        ) {
            sqlx::query(
                "INSERT INTO markets
                    (chain_id, contract_address, market_id, resolver, creator, deadline,
                     metadata_digest, creation_block_number, creation_transaction_hash)
                 VALUES ($1, $2, $3::numeric, $4, $5, 18446744073709551615,
                         $6, 100, $7)",
            )
            .bind(chain_id)
            .bind(contract)
            .bind(market_id)
            .bind(vec![resolver_byte; 20])
            .bind(vec![creator_byte; 20])
            .bind(vec![digest_byte; 32])
            .bind(vec![digest_byte.saturating_add(1); 32])
            .execute(pool)
            .await
            .unwrap();
        }

        insert_market(&pool, chain_id, &contract, "1", 0x11, 0x12, 0x13).await;
        insert_market(&pool, chain_id, &contract, "2", 0x21, 0x22, 0x23).await;
        insert_market(&pool, chain_id, &other_contract, "900", 0x31, 0x32, 0x33).await;
        insert_market(&pool, other_chain_id, &contract, "901", 0x41, 0x42, 0x43).await;

        let yes_pool =
            (alloy::primitives::U256::MAX - alloy::primitives::U256::from(5)).to_string();
        let total_pool = alloy::primitives::U256::MAX.to_string();
        sqlx::query(
            "INSERT INTO market_states
                (chain_id, contract_address, market_id, yes_pool, no_pool, updated_block_number)
             VALUES ($1, $2, 2, $3::numeric, 5, 102)",
        )
        .bind(chain_id)
        .bind(contract)
        .bind(&yes_pool)
        .execute(&pool)
        .await
        .unwrap();
        for (user_byte, yes_stake, no_stake) in
            [(0xaa_u8, yes_pool.as_str(), "5"), (0xbb_u8, "3", "7")]
        {
            sqlx::query(
                "INSERT INTO market_positions
                    (chain_id, contract_address, market_id, user_address,
                     yes_stake, no_stake, updated_block_number)
                 VALUES ($1, $2, 2, $3, $4::numeric, $5::numeric, 103)",
            )
            .bind(chain_id)
            .bind(contract)
            .bind(vec![user_byte; 20])
            .bind(yes_stake)
            .bind(no_stake)
            .execute(&pool)
            .await
            .unwrap();
        }

        let reader = PostgresMarketReader::from_pool(
            pool.clone(),
            u64::try_from(chain_id).unwrap(),
            alloy::primitives::Address::from(contract),
        )
        .unwrap();
        let application = app(Arc::new(reader));
        let list = application
            .clone()
            .oneshot(Request::get("/api/markets").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let list = response_json(list).await;
        let markets = list.as_array().unwrap();
        assert_eq!(markets.len(), 2, "other chain or contract leaked");
        assert_eq!(markets[0]["market_id"], "2");
        assert_eq!(markets[0]["yes_pool"], yes_pool);
        assert_eq!(markets[0]["no_pool"], "5");
        assert_eq!(markets[0]["total_pool"], total_pool);
        assert_eq!(markets[0]["resolver"], format!("0x{}", "21".repeat(20)));
        assert_eq!(
            markets[0]["metadata_digest"],
            format!("0x{}", "23".repeat(32))
        );
        assert_eq!(markets[1]["market_id"], "1");
        assert_eq!(markets[1]["yes_pool"], "0");
        assert_eq!(markets[1]["no_pool"], "0");
        assert_eq!(markets[1]["total_pool"], "0");
        assert!(markets.iter().all(|market| market["market_id"] != "900"));
        assert!(markets.iter().all(|market| market["market_id"] != "901"));

        for hidden_market in ["900", "901"] {
            let response = application
                .clone()
                .oneshot(
                    Request::get(format!("/api/markets/{hidden_market}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        let positions = application
            .oneshot(
                Request::get("/api/markets/2/positions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(positions.status(), StatusCode::OK);
        let positions = response_json(positions).await;
        let positions = positions.as_array().unwrap();
        assert_eq!(positions.len(), 2);
        assert_eq!(
            positions[0]["user_address"],
            format!("0x{}", "aa".repeat(20))
        );
        assert_eq!(positions[0]["yes_stake"], yes_pool);
        assert_eq!(positions[0]["no_stake"], "5");
        assert_eq!(positions[0]["total_stake"], total_pool);
        assert_eq!(positions[1]["yes_stake"], "3");
        assert_eq!(positions[1]["no_stake"], "7");
        assert_eq!(positions[1]["total_stake"], "10");

        sqlx::query("DELETE FROM indexed_blocks WHERE chain_id IN ($1, $2)")
            .bind(chain_id)
            .bind(other_chain_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
