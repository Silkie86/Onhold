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
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;

pub use config::Config;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Largest request body the API accepts, except on upload routes.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Largest body an upload route accepts: a CSV file plus its JSON wrapping.
pub(crate) const MAX_UPLOAD_BODY_BYTES: usize = services::imports::MAX_CSV_BYTES + 64 * 1024;

/// Largest file upload body; the configured limit (25 MB by default) is
/// checked by the service, within this.
pub(crate) const MAX_FILE_BODY_BYTES: usize = 100 * 1024 * 1024;

/// The body limit for a request: uploads get more room than everything else.
fn body_limit(method: &axum::http::Method, path: &str) -> usize {
    if method == axum::http::Method::POST && path == "/api/v1/imports" {
        MAX_UPLOAD_BODY_BYTES
    } else if method == axum::http::Method::POST && path == "/api/v1/files" {
        MAX_FILE_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    }
}

/// Refuses bodies over the route's limit: early from Content-Length, and
/// while reading for bodies that do not declare one.
async fn limit_body(
    req: Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let limit = body_limit(req.method(), req.uri().path());
    let declared = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|len| len > limit) {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        )
            .into_response();
    }
    let (parts, body) = req.into_parts();
    let body = axum::body::Body::new(http_body_util::Limited::new(body, limit));
    next.run(Request::from_parts(parts, body)).await
}

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
    /// Where new files' bytes go.
    pub storage: Arc<services::files::Storage>,
    /// How documents become PDFs.
    pub renderer: Arc<dyn core_pdf::write::Renderer>,
}

/// The PDF renderer the configuration asks for: Chromium when it can be
/// found (or is named), else the plain-text layout.
fn renderer(pdf: &config::PdfConfig) -> Arc<dyn core_pdf::write::Renderer> {
    use core_pdf::write::{Chromium, PlainText};
    match pdf {
        config::PdfConfig::Plain => Arc::new(PlainText),
        config::PdfConfig::Chromium { path, no_sandbox } => {
            let mut c = Chromium::new(path.clone());
            c.no_sandbox = *no_sandbox;
            Arc::new(c)
        }
        config::PdfConfig::Auto { no_sandbox } => match Chromium::find() {
            Some(mut c) => {
                c.no_sandbox = *no_sandbox;
                tracing::info!(browser = %c.program.display(), "documents print with Chromium");
                Arc::new(c)
            }
            None => {
                tracing::warn!("no Chromium found: documents print in the plain layout (set CHROMIUM_PATH)");
                Arc::new(PlainText)
            }
        },
    }
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Result<Self, error::ApiError> {
        let oidc = Arc::new(oidc::Registry::new(config.oidc.clone())?);
        let storage = Arc::new(services::files::Storage::from_config(&config.storage)?);
        let renderer = renderer(&config.pdf);
        Ok(Self {
            db,
            config: Arc::new(config),
            oidc,
            events: Arc::default(),
            storage,
            renderer,
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
        (name = "talent", description = "Artists, acts, rooms and kit you represent or book"),
        (name = "organisations", description = "Venues, promoters, brands and other companies"),
        (name = "contacts", description = "People you deal with"),
        (name = "search", description = "Finding records across the CRM"),
        (name = "views", description = "Saved table views"),
        (name = "imports", description = "Bringing records in from spreadsheets"),
        (name = "enquiries", description = "Requests for talent, through the enquiry pipeline"),
        (name = "bookings", description = "Holds and bookings, with clash checks"),
        (name = "quotes", description = "Priced offers the buyer accepts on the web"),
        (name = "calendar", description = "The calendar, availability and iCal feeds"),
        (name = "tasks", description = "To-dos on any record"),
        (name = "documents", description = "Files, documents, e-signature and past PDFs"),
        (name = "finance", description = "Finance settings, tax codes and exchange rates"),
        (name = "invoices", description = "Invoices and credit notes"),
        (name = "payments", description = "Payments received, refunds and payouts"),
        (name = "settlements", description = "What each talent is owed for a booking"),
        (name = "reports", description = "Revenue, what is owed, cash and the funnel"),
        (name = "public", description = "Pages reached by secret link, without an account"),
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
            .merge(routes::crm::router())
            .merge(routes::bookings::router())
            .merge(routes::documents::router())
            .merge(routes::finance::router())
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
    let cors = cors(&state.config.cors_origins);

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
                .layer(cors)
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                ))
                .layer(axum::middleware::from_fn(limit_body)),
        )
}

/// Cross-origin access for the desktop and mobile apps, whose pages are served
/// from their own origins (see [`config::APP_ORIGINS`]). Credentials are not
/// allowed: the apps send bearer tokens, so a browser never attaches the
/// session cookie to a cross-origin request.
fn cors(origins: &[String]) -> CorsLayer {
    use axum::http::{HeaderValue, Method, header};
    let origins: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::IF_MATCH,
            HeaderName::from_static(extract::WORKSPACE_HEADER),
            HeaderName::from_static(idempotency::HEADER),
            REQUEST_ID_HEADER,
        ])
        .expose_headers([header::ETAG, header::LOCATION, REQUEST_ID_HEADER])
        .max_age(Duration::from_secs(3600))
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
