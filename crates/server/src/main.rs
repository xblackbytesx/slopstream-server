use anyhow::{Result, bail};
use slopstream_server::auth::Role;
use slopstream_server::config::Config;
use slopstream_server::{AppState, MIGRATOR, router, users};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  slopstream-server [serve]
  slopstream-server create-admin <email>       (then set its password)
  slopstream-server set-password <email>       (reads the password from stdin)
  slopstream-server new-token <email> <label>  (an /api/v1 token, shown once)";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cfg = Config::from_env()?;
    let pool = PgPoolOptions::new().max_connections(10).connect(&cfg.database_url).await?;
    MIGRATOR.run(&pool).await?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => {
            let state = AppState {
                cookie_secure: cfg.cookie_secure,
                public_url: cfg.public_url.clone(),
                ..AppState::new(pool, &cfg.secret_key)
            };
            tokio::spawn(slopstream_server::sessions::run_checker(state.clone()));
            let app = router(state);
            let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
            tracing::info!(bind = %cfg.bind, "listening");
            axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
        }
        ["create-admin", email] => {
            let id = users::create(&pool, email, Role::Admin).await?;
            println!("created admin {email} ({id}); now run set-password {email}");
        }
        ["set-password", email] => {
            // stdin, never argv: argv lands in shell history and `ps`.
            eprint!("New password for {email}: ");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            users::set_password(&pool, email, line.trim_end_matches(['\r', '\n'])).await?;
            println!("password set; web sessions for {email} were ended");
        }
        ["new-token", email, label] => {
            let id = users::id_by_email(&pool, email).await?;
            let token = users::new_token(&pool, id, label, false).await?;
            println!("{token}\n(shown once)");
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
