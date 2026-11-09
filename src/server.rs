use crate::{
    search::{Library, Results, SearchRequest},
    signal::{self, Invalid, PeakRequest},
};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderValue, Method, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use tokio::{net::TcpListener, sync::Semaphore};

const MAX_BODY: usize = 3_000_000;

#[derive(Clone)]
struct App {
    library: Library,
    search_slot: Arc<Semaphore>,
    hostname: String,
}

#[derive(Debug, thiserror::Error)]
#[error("A library search is already running.")]
struct Busy;

impl App {
    async fn search(&self, input: SearchRequest) -> Result<Results> {
        let permit = self
            .search_slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| Busy)?;
        let library = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            library.search(input)
        })
        .await?
    }
}

fn api_error(error: anyhow::Error) -> Response {
    let (status, detail) = if error.is::<Invalid>() {
        (StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
    } else if error.is::<Busy>() {
        (StatusCode::TOO_MANY_REQUESTS, error.to_string())
    } else {
        eprintln!("Raman request failed: {error:#}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The Raman request failed.".into(),
        )
    };
    (status, Json(json!({ "detail": detail }))).into_response()
}

async fn status(State(app): State<App>) -> Response {
    app.library
        .status()
        .map(Json)
        .map_or_else(api_error, IntoResponse::into_response)
}

fn json_error(error: JsonRejection) -> Response {
    (error.status(), Json(json!({"detail": error.body_text()}))).into_response()
}

async fn detect(input: Result<Json<PeakRequest>, JsonRejection>) -> Response {
    let Json(input) = match input {
        Ok(input) => input,
        Err(error) => return json_error(error),
    };
    tokio::task::spawn_blocking(move || signal::peaks(input))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|peaks| Ok(peaks?))
        .map(|peaks| Json(json!({ "peaks": peaks })))
        .map_or_else(api_error, IntoResponse::into_response)
}

async fn search(
    State(app): State<App>,
    input: Result<Json<SearchRequest>, JsonRejection>,
) -> Response {
    let Json(input) = match input {
        Ok(input) => input,
        Err(error) => return json_error(error),
    };
    app.search(input)
        .await
        .map(Json)
        .map_or_else(api_error, IntoResponse::into_response)
}

async fn http_policy(
    State(app): State<App>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.method() == Method::POST
        && let Some(origin) = request.headers().get(header::ORIGIN)
    {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let allowed = host.parse::<Authority>().ok().is_some_and(|a| {
            let name = a.host().trim_matches(['[', ']']);
            name == "localhost"
                || name.parse::<IpAddr>().is_ok()
                || name.eq_ignore_ascii_case(&format!("{}.local", app.hostname))
        });
        if !allowed || origin != format!("http://{host}").as_str() {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"detail": "Use the local Raman site."})),
            )
                .into_response();
        }
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"
    ));
    response
}

fn api_schema() -> Value {
    let array =
        json!({"type": "array", "minItems": 8, "maxItems": 65536, "items": {"type": "number"}});
    let request = |extra: Value| {
        let mut properties = extra.as_object().unwrap().clone();
        properties.insert("wavenumber".into(), array.clone());
        properties.insert("intensity".into(), array.clone());
        json!({
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object", "required": ["wavenumber", "intensity"], "properties": properties
            }}}},
            "responses": {"200": {"description": "Raman analysis"}}
        })
    };
    json!({
        "openapi": "3.1.0", "info": {"title": "SU(3) Raman", "version": env!("CARGO_PKG_VERSION")},
        "paths": {
            "/api/status": {"get": {"responses": {"200": {"description": "Device and library status"}}}},
            "/api/peaks": {"post": request(json!({
                "min_prominence": {"type": "number", "exclusiveMinimum": 0, "maximum": 1, "default": 0.03},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 30}
            }))},
            "/api/search": {"post": request(json!({
                "kind": {"enum": ["measured", "computed", "both"], "default": "measured"},
                "sources": {"type": "array", "items": {"type": "string"}},
                "limit": {"type": "integer", "minimum": 1, "maximum": 30, "default": 10},
                "mode": {"enum": ["auto", "indexed", "exact"], "default": "auto"}
            }))}
        }
    })
}

#[derive(Clone)]
struct Agent {
    app: App,
    router: ToolRouter<Self>,
}

fn tool_error(error: anyhow::Error) -> ErrorData {
    if error.is::<Invalid>() {
        ErrorData::invalid_params(error.to_string(), None)
    } else if error.is::<Busy>() {
        ErrorData::internal_error(error.to_string(), None)
    } else {
        eprintln!("Raman tool failed: {error:#}");
        ErrorData::internal_error("The Raman request failed.", None)
    }
}

#[tool_router]
impl Agent {
    #[tool(description = "List installed reference collections and the supported device mode.")]
    fn raman_status(&self) -> Result<CallToolResult, ErrorData> {
        self.app
            .library
            .status()
            .map(CallToolResult::structured)
            .map_err(tool_error)
    }

    #[tool(
        description = "Detect peaks from calibrated Raman shifts in cm⁻¹ and paired intensities."
    )]
    async fn detect_peaks(
        &self,
        Parameters(input): Parameters<PeakRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        tokio::task::spawn_blocking(move || signal::peaks(input))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|peaks| Ok(peaks?))
            .map(|peaks| CallToolResult::structured(json!({ "peaks": peaks })))
            .map_err(tool_error)
    }

    #[tool(
        description = "Rank Raman reference spectra. Measured and calculated matches are returned separately."
    )]
    async fn search_library(
        &self,
        Parameters(input): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let results = self.app.search(input).await.map_err(tool_error)?;
        serde_json::to_value(results)
            .map(CallToolResult::structured)
            .map_err(|error| tool_error(error.into()))
    }
}

#[tool_handler(router = self.router)]
impl ServerHandler for Agent {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

pub async fn serve(library: Library, http_addr: SocketAddr, mcp_addr: SocketAddr) -> Result<()> {
    anyhow::ensure!(mcp_addr.ip().is_loopback(), "MCP must bind to loopback");
    let hostname = hostname::get()?
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or("localhost")
        .to_owned();
    let app = App {
        library,
        search_slot: Arc::new(Semaphore::new(1)),
        hostname,
    };
    let http = Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../runtime/index.html")) }),
        )
        .route(
            "/site.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../runtime/site.css"),
                )
            }),
        )
        .route(
            "/site.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../runtime/site.js"),
                )
            }),
        )
        .route("/openapi.json", get(|| async { Json(api_schema()) }))
        .route("/api/status", get(status))
        .route("/api/peaks", post(detect))
        .route("/api/search", post(search))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(app.clone(), http_policy))
        .with_state(app.clone());

    let service: StreamableHttpService<Agent, LocalSessionManager> = StreamableHttpService::new(
        move || {
            Ok(Agent {
                app: app.clone(),
                router: Agent::tool_router(),
            })
        },
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(MAX_BODY)
            .with_allowed_origins(["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"]),
    );
    let mcp = Router::new().nest_service("/mcp", service);
    let http_listener = TcpListener::bind(http_addr).await?;
    let mcp_listener = TcpListener::bind(mcp_addr).await?;
    eprintln!(
        "Raman API at {}, MCP at {}",
        http_listener.local_addr()?,
        mcp_listener.local_addr()?
    );
    tokio::try_join!(async { axum::serve(http_listener, http).await }, async {
        axum::serve(mcp_listener, mcp).await
    })?;
    Ok(())
}
