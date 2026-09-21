use anyhow::{Context, Result};

use crate::{config, graphite::Graphite};

pub fn run(token: Option<&str>) -> Result<()> {
    let active_token = match token {
        Some(token) => config::validate_token(token)?.to_owned(),
        None => config::graphite_token()?.context(
            "no Graphite token configured; get one at https://app.graphite.com/activate and run `jj-gt auth --token <TOKEN>`",
        )?,
    };
    let graphite = Graphite::new(active_token.clone())?;
    let status = graphite.check_auth(None)?;
    let login = status
        .github_login
        .filter(|login| !login.is_empty())
        .context("Graphite did not return an authenticated GitHub login")?;
    if token.is_some() {
        let path = config::path()?;
        config::save_token(&path, &active_token)?;
        println!("Saved Graphite auth token to {}", path.display());
        if std::env::var_os("GRAPHITE_AUTH_TOKEN").is_some() {
            println!("GRAPHITE_AUTH_TOKEN overrides the saved token for subsequent commands.");
        }
    }
    println!("Authenticated with Graphite as {login}");
    Ok(())
}
