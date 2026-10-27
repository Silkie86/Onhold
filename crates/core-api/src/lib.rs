//! The HTTP API: routing, middleware, auth, the service layer and the
//! OpenAPI document generated from the handlers themselves.

pub mod config;
pub mod error;
pub mod extract;
pub mod health;
pub mod idempotency;
pub mod oidc;
pub mod password;
pub mod routes;
pub mod services;
pub mod totp;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, Request};
use axum::routing::get;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tower_http::LatencyUnit;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;

pub use config::Config;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Largest request body the API accepts. Uploads get their own, larger limit
/// on their own routes when documents arrive in M5.
const MAX_BODY_BYTES: usize = 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    /// The app pool: every connection runs as `onhold_app`, under row-level security.
    pub db: PgPool,
    pub config: Arc<Config>,
    pub oidc: oidc::SharedRegistry,
    /// Live event notifications for server-sent event streams.
    pub events: Arc<services::events::Hub>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Result<Self, error::ApiError> {
        let oidc = Arc::new(oidc::Registry::new(config.oidc.clone())?);
        Ok(Self {
            db,
            config: Arc::new(config),
            oidc,
            events: Arc::default(),
        })
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "OnHold API",
        description = "The OnHold platform core. The web app uses exactly this API; nothing is hidden.\n\n\
            Authenticate with the session cookie (browsers; send `X-CSRF-Token` on writes) or \
            `Authorization: Bearer ohk_…` (API keys). Workspace routes need `X-Workspace-Id` \
            unless an API key implies it. Errors are `{\"error\": {\"code\", \"message\", \"field\"?}}`. \
            Writes accept `Idempotency-Key` for safe retries; records with a `version` accept `If-Match`."
    ),
    modifiers(&SecuritySchemes),
    tags(
        (name = "auth", description = "Signing in and account security"),
        (name = "workspace", description = "Workspace settings and roles"),
        (name = "members", description = "Who has access"),
        (name = "invitations", description = "Inviting people"),
        (name = "api-keys", description = "Keys for scripts and integrations"),
        (name = "audit", description = "The audit log"),
        (name = "custom-fields", description = "Extra fields on records"),
        (name = "pipelines", description = "Stages records move through"),
        (name = "templates", description = "Message templates and previews"),
        (name = "rules", description = "Automation rules"),
        (name = "things", description = "The test entity the engines are built against"),
        (name = "events", description = "What changed, in order: the sync feed"),
        (name = "webhooks", description = "Events pushed to your URLs"),
        (name = "jobs", description = "Background work"),
        (name = "messages", description = "Messages sent"),
        (name = "admin", description = "Operator tools (superadmin)"),
        (name = "health", description = "Probes"),
    )
)]
struct ApiDoc;

struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(extract::SESSION_COOKIE))),
        );
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("ohk_<id>_<secret>")
                    .build(),
            ),
        );
    }
}

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi()).nest(
        "/api/v1",
        OpenApiRouter::new()
            .merge(routes::auth::router())
            .merge(routes::workspace::router())
            .merge(routes::config::router())
            .merge(routes::things::router())
            .merge(routes::events::router())
            .merge(routes::webhooks::router())
            .merge(routes::jobs::router())
            .merge(routes::admin::router()),
    )
}

/// The OpenAPI 3.1 document, generated from the same handlers the router serves.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let (_, mut api) = api_router()
        .merge(health::openapi_router())
        .split_for_parts();
    api.info.version = env!("CARGO_PKG_VERSION").to_owned();
    api
}

/// The full API router with its middleware stack.
pub fn router(state: AppState) -> Router {
    let (api, _) = api_router().split_for_parts();
    let spec = Arc::new(openapi());

    Router::new()
        .merge(health::router())
        .merge(api)
        .route(
            "/api/openapi.json",
            get(move || std::future::ready(axum::Json(spec.as_ref().clone()))),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            idempotency::layer,
        ))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|req: &Request<_>| {
                            let request_id = req
                                .headers()
                                .get(REQUEST_ID_HEADER)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default();
                            tracing::info_span!(
                                "request",
                                method = %req.method(),
                                path = %req.uri().path(),
                                request_id,
                                workspace_id = tracing::field::Empty,
                                user_id = tracing::field::Empty,
                                api_key_id = tracing::field::Empty,
                            )
                        })
                        .on_response(DefaultOnResponse::new().latency_unit(LatencyUnit::Millis)),
                )
                .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                ))
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
}

/// Serves `app` on `addr` until `shutdown` is cancelled, then drains in-flight requests.
pub async fn serve(
    addr: SocketAddr,
    app: Router,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await
}
