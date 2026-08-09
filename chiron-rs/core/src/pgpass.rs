use anyhow::{Context, Result};
use tokio_postgres::config::Host;
use tokio_postgres::Config;

/// Look up a password in a libpq-style `.pgpass` file:
/// `hostname:port:database:username:password`, one entry per line, `*`
/// matches any value. Returns the first matching entry's password.
fn pgpass_lookup(host: &str, port: u16, dbname: &str, user: &str) -> Option<String> {
    let path = std::env::var("PGPASSFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.pgpass")))?;
    let content = std::fs::read_to_string(path).ok()?;
    let port = port.to_string();
    let matches = |field: &str, value: &str| field == "*" || field == value;

    content.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let parts: Vec<&str> = line.splitn(5, ':').collect();
        let [h, p, d, u, pw] = parts[..] else { return None };
        (matches(h, host) && matches(p, &port) && matches(d, dbname) && matches(u, user))
            .then(|| pw.to_string())
    })
}

/// Parse DB_URL into a `tokio_postgres::Config`, resolving a missing
/// password via `~/.pgpass` first. tokio-postgres, unlike libpq/psql, does
/// NOT perform this resolution itself — connecting with a password-less URL
/// fails with "invalid configuration: password missing" even when a
/// matching `.pgpass` entry exists.
pub fn resolve_config(db_url: &str) -> Result<Config> {
    let mut config: Config = db_url.parse().context("invalid database URL")?;
    if config.get_password().is_none() {
        let host = config
            .get_hosts()
            .first()
            .and_then(|h| match h {
                Host::Tcp(s) => Some(s.clone()),
                #[allow(unreachable_patterns)]
                _ => None,
            })
            .unwrap_or_else(|| "localhost".to_string());
        let port = config.get_ports().first().copied().unwrap_or(5432);
        let dbname = config.get_dbname().unwrap_or_default().to_string();
        let user = config.get_user().unwrap_or_default().to_string();
        if let Some(password) = pgpass_lookup(&host, port, &dbname, &user) {
            config.password(password);
        }
    }
    Ok(config)
}
