use {
    axum::{
        body::Bytes,
        http::Request,
        middleware::{self},
        response::Response,
        routing::{get, post},
        Router,
    },
    secrecy::ExposeSecret,
    sqlx::postgres::PgPoolOptions,
    std::{future::ready, net::SocketAddr, time::Duration},
    tower_http::{classify::ServerErrorsFailureClass, trace::TraceLayer},
    tracing::Span,
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
};

use newsletter::configuration::get_configuration;
use newsletter::graceful_shutdown::shutdown_signal;
use newsletter::metrics::{setup_metrics_recorder, track_metrics};
use newsletter::routes::{global_404, health_check, subscriptions};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "newsletter=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let recorder_handle = setup_metrics_recorder();

    let configuration = get_configuration().expect("Failed to read config");

    let db_pool = PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy_with(configuration.database.connection_string());

    let app = Router::with_state(db_pool)
        .route("/health_check", get(health_check))
        .route("/subscriptions", post(subscriptions))
        .fallback(global_404)
        .route("/metrics", get(move || ready(recorder_handle.render())))
        .route_layer(middleware::from_fn(track_metrics))
        .layer(TraceLayer::new_for_http())
        .layer(
            TraceLayer::new_for_http()
                .on_request(|request: &Request<_>, _span: &Span| {
                    tracing::debug!("st`arted {} {}", request.method(), request.uri().path())
                })
                .on_response(|_response: &Response, latency: Duration, _span: &Span| {
                    tracing::debug!("response generated in {:?}", latency)
                })
                .on_body_chunk(|chunk: &Bytes, _latency: Duration, _span: &Span| {
                    tracing::debug!("sending {} bytes", chunk.len())
                })
                .on_failure(
                    |error: ServerErrorsFailureClass, _latency: Duration, _span: &Span| {
                        tracing::error!("something went wrong {:#?}", error)
                    },
                ),
        );

    let address = format!(
        "{}:{}",
        configuration.application.host, configuration.application.port
    );

    println!(
        "SMTP EMAIL KEY -> {:?}",
        configuration.email.key.expose_secret()
    );

    let address: SocketAddr = address.parse().expect("Unable to parse socket address");

    tracing::debug!("listing on address {address}");

    axum::Server::bind(&address)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
