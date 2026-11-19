//! Profile selection happens before starting a runtime. A worker, its auth refreshes,
//! its session history and any reset redemption all stay in the same Codex home.
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand as Commands};
use codex_core::config::{Config, find_codex_home};
use codex_login::{AuthDotJson, AuthManager, load_auth_dot_json, save_auth};
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;
use serde::{Deserialize, Serialize};

use super::Subcommand;

const ROOT: &str = "CODEX_INTERNAL_CX_ACCOUNTS_ROOT";
const PIN: &str = "CODEX_INTERNAL_CX_ACCOUNT_NAME";
const MAX_TRANSFER: u64 = 128 * 1024;

#[derive(Debug, Args)]
pub(super) struct Options {
    /// Use an isolated named account, the original "default", or quota-based "auto".
    #[arg(long, value_name = "NAME", global = true, value_parser = account_name)]
    account: Option<String>,
    /// Write the current managed ChatGPT login to stdout for transfer to another device.
    #[arg(long, conflicts_with_all = ["import_auth", "prompt"])]
    export_auth: bool,
    /// Read a managed ChatGPT login from stdin and save it in the selected credential store.
    #[arg(long, conflicts_with = "prompt")]
    import_auth: bool,
    #[arg(long, hide = true, conflicts_with_all = ["import_auth", "export_auth"])]
    account_status_json: bool,
}

#[derive(Debug, Args)]
pub(super) struct Accounts {
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Commands)]
enum Action {
    /// Add a named account and start its login.
    Add {
        #[arg(value_parser = account_name)]
        name: String,
    },
    /// Sign out and remove a named account from selection, keeping its local history.
    Remove {
        #[arg(value_parser = account_name)]
        name: String,
    },
    /// Sign in to a profile.
    Login {
        #[arg(value_parser = account_name)]
        name: String,
    },
    /// Sign out of a profile without removing it.
    Logout {
        #[arg(value_parser = account_name)]
        name: String,
    },
    /// Show remaining quota, reset times and available reset credits.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print which account a new job would use.
    Current,
    /// Select the default account for future invocations; use "auto" for rotation.
    Use {
        #[arg(value_parser = account_name)]
        name: String,
    },
}

fn account_name(name: &str) -> std::result::Result<String, String> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err("use 1–32 lowercase letters, digits, underscores or hyphens".into());
    }
    Ok(name.to_owned())
}

fn home(root: &Path, name: &str) -> PathBuf {
    if name == "default" {
        root.to_owned()
    } else {
        root.join("cx-accounts").join(format!("profile-{name}"))
    }
}

fn read_selection(root: &Path) -> Result<Option<String>> {
    match fs::read_to_string(root.join("cx-accounts/selection")) {
        Ok(name) => Ok(Some(account_name(name.trim()).map_err(anyhow::Error::msg)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn names(root: &Path) -> Result<Vec<String>> {
    let mut names = vec!["default".to_owned()];
    match fs::read_dir(root.join("cx-accounts")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && let Some(name) = entry
                        .file_name()
                        .to_str()
                        .and_then(|s| s.strip_prefix("profile-"))
                        .and_then(|s| account_name(s).ok())
                    && name != "default"
                    && name != "auto"
                    && !entry.path().join("cx-account-removed").try_exists()?
                {
                    names.push(name);
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    names[1..].sort();
    Ok(names)
}

fn select(root: &Path, name: &str) -> Result<()> {
    ensure!(
        name == "auto" || names(root)?.iter().any(|n| n == name),
        "Unknown account {name}"
    );
    let dir = root.join("cx-accounts");
    fs::create_dir_all(&dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    writeln!(tmp, "{name}")?;
    tmp.as_file().sync_all()?;
    tmp.persist(dir.join("selection"))?;
    Ok(())
}

fn profile_command(root: &Path, name: &str) -> Result<Command> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.env_remove(codex_login::CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(codex_login::CODEX_API_KEY_ENV_VAR)
        .env(ROOT, root)
        .env(PIN, name)
        .env("CODEX_HOME", home(root, name));
    if name != "default" {
        cmd.env_remove(codex_state::SQLITE_HOME_ENV);
    }
    Ok(cmd)
}

fn create_home(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn launch(root: &Path, name: &str, create: bool, remote_control_disabled: bool) -> Result<()> {
    let path = home(root, name);
    if create {
        create_home(&path)?;
    }
    ensure!(
        path.is_dir(),
        "Unknown account {name}. Create it with cx --account {name} login or --import-auth"
    );
    let mut cmd = profile_command(root, name)?;
    if remote_control_disabled {
        cmd.env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1");
    }
    cmd.args(std::env::args_os().skip(1));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.exec().into())
    }
    #[cfg(not(unix))]
    std::process::exit(cmd.status()?.code().unwrap_or(1));
}

pub(super) async fn dispatch(
    options: &Options,
    command: Option<&Subcommand>,
    overrides: &CliConfigOverrides,
    remote_control_disabled: bool,
    uses_remote_account: bool,
) -> Result<bool> {
    if uses_remote_account {
        ensure!(
            options.account.is_none()
                && !options.export_auth
                && !options.import_auth
                && !options.account_status_json
                && !matches!(command, Some(Subcommand::Accounts(_))),
            "Account profiles are local to the device running the server"
        );
        return Ok(false);
    }
    let current = match find_codex_home() {
        Ok(home) => home,
        Err(_)
            if options.account.is_none()
                && !options.import_auth
                && !options.export_auth
                && !options.account_status_json
                && !matches!(command, Some(Subcommand::Accounts(_))) =>
        {
            return Ok(false);
        }
        Err(err) => return Err(err.into()),
    };
    let inherited = std::env::var_os(ROOT)
        .map(PathBuf::from)
        .zip(std::env::var(PIN).ok())
        .filter(|(root, name)| {
            account_name(name).is_ok()
                && home(root, name).canonicalize().is_ok_and(|path| {
                    current
                        .as_path()
                        .canonicalize()
                        .is_ok_and(|active| active == path)
                })
        });
    let root = inherited
        .as_ref()
        .map_or(current.as_path(), |(root, _)| root.as_path());
    let transfer = options.import_auth || options.export_auth;
    ensure!(
        !transfer || command.is_none(),
        "Use auth transfer flags without a subcommand"
    );
    // The standalone package is shared by every account.
    if matches!(command, Some(Subcommand::Update)) && !options.account_status_json {
        return Ok(false);
    }
    if let Some(Subcommand::Accounts(accounts)) = command {
        manage(root, accounts, overrides).await?;
        return Ok(true);
    }
    let pinned = inherited.as_ref().map(|(_, name)| name.as_str());
    let selection = if options.account.is_none() && pinned.is_none() {
        read_selection(root)?
    } else {
        None
    };
    let requested = options
        .account
        .as_deref()
        .or(pinned)
        .or(selection.as_deref());
    if let Some(name) = requested {
        let chosen = if name == "auto" {
            if let Some(pin) = pinned {
                pin.to_string()
            } else {
                ensure!(
                    !transfer
                        && !matches!(command, Some(Subcommand::Login(_) | Subcommand::Logout(_))),
                    "Select a specific account with --account NAME"
                );
                ensure!(
                    command.is_none()
                        || matches!(command, Some(Subcommand::Review(_)))
                        || matches!(command, Some(Subcommand::Exec(exec)) if matches!(exec.command, None | Some(codex_exec::Command::Review(_)))),
                    "Use --account NAME for commands acting on existing account state"
                );
                let name = choose(&statuses(root, overrides).await?)?;
                eprintln!("Using account {name}");
                name
            }
        } else {
            name.to_string()
        };
        let removed = home(root, &chosen).join("cx-account-removed");
        let login = matches!(command, Some(Subcommand::Login(options)) if options.action.is_none());
        if chosen != "default" && removed.try_exists()? {
            ensure!(
                options.import_auth || login,
                "Account {chosen} has been removed"
            );
            if login && pinned != Some(chosen.as_str()) {
                let mut cmd = profile_command(root, &chosen)?;
                if remote_control_disabled {
                    cmd.env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1");
                }
                let status = cmd.args(std::env::args_os().skip(1)).status()?;
                ensure!(status.success(), "Login failed for {chosen}");
                fs::remove_file(removed)?;
                return Ok(true);
            }
        }
        if pinned != Some(chosen.as_str()) {
            launch(
                root,
                &chosen,
                options.import_auth || login,
                remote_control_disabled,
            )?;
        }
    }
    if !transfer && !options.account_status_json {
        return Ok(false);
    }
    let config = codex_cli::cloud_config::load_config(overrides, Default::default()).await?;
    if options.account_status_json {
        serde_json::to_writer(io::stdout().lock(), &quota(&config).await?)?;
    } else {
        transfer_auth(&config, options.import_auth)?;
        if options.import_auth && pinned.is_some_and(|name| name != "default") {
            let marker = current.join("cx-account-removed");
            if marker.try_exists()? {
                fs::remove_file(marker)?;
            }
        }
    }
    Ok(true)
}

fn portable(mut doc: AuthDotJson, config: &Config) -> Result<AuthDotJson> {
    ensure!(
        matches!(doc.auth_mode, None | Some(AuthMode::Chatgpt))
            && doc.openai_api_key.is_none()
            && doc.personal_access_token.is_none()
            && doc.bedrock_api_key.is_none()
            && doc.bedrock_access_keys.is_none(),
        "Expected a managed ChatGPT account login"
    );
    let tokens = doc.tokens.as_mut().context("Missing account tokens")?;
    ensure!(
        !tokens.refresh_token.is_empty() && !tokens.access_token.is_empty(),
        "Incomplete account login"
    );
    let access = codex_login::token_data::parse_chatgpt_jwt_claims(&tokens.access_token)
        .map_err(|_| anyhow::anyhow!("Invalid access token"))?;
    let account = tokens
        .account_id
        .as_deref()
        .or(tokens.id_token.chatgpt_account_id.as_deref())
        .context("Missing account identity")?;
    ensure!(
        !account.is_empty()
            && access.chatgpt_account_id.as_deref() == Some(account)
            && access.chatgpt_user_id == tokens.id_token.chatgpt_user_id,
        "Token identities do not match"
    );
    let policy = config.auth_config();
    ensure!(
        policy.is_login_method_allowed(ForcedLoginMethod::Chatgpt)
            && policy
                .effective_chatgpt_workspaces()
                .is_none_or(|ids| ids.iter().any(|id| id == account)),
        "Account is excluded by the configured login policy"
    );
    // Workspace switching preserves the original ID token and changes the access token.
    tokens.account_id = Some(account.to_owned());
    doc.agent_identity = None;
    doc.auth_mode = Some(AuthMode::Chatgpt);
    Ok(doc)
}

fn transfer_auth(config: &Config, import: bool) -> Result<()> {
    let mode = config.cli_auth_credentials_store_mode;
    let backend = config.auth_keyring_backend_kind();
    if import {
        ensure!(
            mode != codex_login::AuthCredentialsStoreMode::Ephemeral,
            "Import requires a persistent credential store"
        );
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_TRANSFER + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_TRANSFER,
            "Account document too large"
        );
        let doc = serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|_| anyhow::anyhow!("Invalid account document"))?;
        save_auth(&config.codex_home, &portable(doc, config)?, mode, backend)?;
        eprintln!("Account imported.");
    } else {
        let doc =
            load_auth_dot_json(&config.codex_home, mode, backend)?.context("No saved login")?;
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &portable(doc, config)?)?;
        writeln!(output)?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Quota {
    email: Option<String>,
    account_id: String,
    remaining_percent: Option<f64>,
    unlimited: bool,
    resets_at: Option<i64>,
    reset_credits: Option<i64>,
    available: bool,
}

async fn quota(config: &Config) -> Result<Quota> {
    let manager = AuthManager::shared_from_auth_config_with_auth_modes(
        config.auth_config(),
        false,
        vec![AuthMode::Chatgpt],
    )
    .await?;
    let (auth, factory) = manager
        .auth_with_http_client_factory()
        .await
        .context("No account login")?;
    ensure!(
        config.auth_config().allows_auth(&auth),
        "Account excluded by policy"
    );
    let account_id = auth.get_account_id().context("Missing account identity")?;
    let user_id = auth.get_chatgpt_user_id();
    let cache_path = config.codex_home.join("cx-account-quota.json");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.codex_home.join("cx-account-quota.lock"))?;
    lock.lock()?;
    let now = chrono::Utc::now().timestamp();
    if let Some(cache) = fs::read(&cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CachedQuota>(&bytes).ok())
        && (0..30).contains(&(now.saturating_sub(cache.checked_at)))
        && cache.backend == config.chatgpt_base_url
        && cache.user_id == user_id
        && cache.quota.account_id == account_id
        && cache.quota.resets_at.is_none_or(|reset| reset > now)
    {
        return Ok(cache.quota);
    }
    let client = codex_backend_client::Client::from_auth(&config.chatgpt_base_url, &auth, factory);
    let usage = client.get_rate_limits_with_reset_credits().await?;
    ensure!(
        usage.account_id.as_ref().is_none_or(|id| id == &account_id),
        "Quota belongs to a different account"
    );
    ensure!(
        usage
            .user_id
            .as_ref()
            .is_none_or(|id| Some(id) == user_id.as_ref()),
        "Quota belongs to a different user"
    );
    let bucket = usage
        .rate_limits
        .iter()
        .find(|s| s.limit_id.as_deref() == Some("codex"))
        .or(usage.rate_limits.first())
        .context("No quota data")?;
    let window = [bucket.primary.as_ref(), bucket.secondary.as_ref()]
        .into_iter()
        .flatten()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
    let remaining_percent = window.map(|w| (100.0 - w.used_percent).clamp(0.0, 100.0));
    let unlimited = bucket
        .credits
        .as_ref()
        .is_some_and(|credits| credits.unlimited);
    let available = usage
        .ordinary_usage_allowed
        .unwrap_or_else(|| unlimited || remaining_percent.is_some_and(|v| v > 0.0))
        && bucket.spend_control_reached != Some(true)
        && bucket.rate_limit_reached_type.is_none();
    let quota = Quota {
        email: auth.get_account_email(),
        account_id,
        remaining_percent,
        unlimited,
        resets_at: window.and_then(|w| w.resets_at),
        reset_credits: usage.rate_limit_reset_credits.map(|v| v.available_count),
        available,
    };
    let mut tmp = tempfile::NamedTempFile::new_in(&config.codex_home)?;
    let cache = CachedQuota {
        checked_at: now,
        backend: config.chatgpt_base_url.clone(),
        user_id,
        quota,
    };
    serde_json::to_writer(&mut tmp, &cache)?;
    tmp.persist(cache_path)?;
    Ok(cache.quota)
}

#[derive(Serialize, Deserialize)]
struct CachedQuota {
    checked_at: i64,
    backend: String,
    user_id: Option<String>,
    quota: Quota,
}

#[derive(Serialize)]
struct Row {
    name: String,
    quota: Option<Quota>,
}

async fn statuses(root: &Path, overrides: &CliConfigOverrides) -> Result<Vec<Row>> {
    let reads = names(root)?.into_iter().map(|name| async move {
        let mut cmd = profile_command(root, &name)?;
        cmd.env("TOKIO_WORKER_THREADS", "1")
            .arg("--account-status-json")
            .args(
                overrides
                    .raw_overrides
                    .iter()
                    .flat_map(|value| ["-c", value.as_str()]),
            );
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let quota =
            match tokio::time::timeout(std::time::Duration::from_secs(12), cmd.output()).await {
                Ok(Ok(output)) if output.status.success() => {
                    serde_json::from_slice(&output.stdout).ok()
                }
                _ => None,
            };
        Ok::<_, anyhow::Error>(Row { name, quota })
    });
    futures::future::try_join_all(reads).await
}

fn choose(rows: &[Row]) -> Result<String> {
    rows.iter()
        .filter_map(|row| Some((row.name.as_str(), row.quota.as_ref()?)))
        .filter(|(_, quota)| quota.available)
        .max_by(|(_, a), (_, b)| {
            a.unlimited.cmp(&b.unlimited).then_with(|| {
                a.remaining_percent
                    .unwrap_or(0.0)
                    .total_cmp(&b.remaining_percent.unwrap_or(0.0))
            })
        })
        .map(|(name, _)| name.to_owned())
        .context(
            "No account has confirmed available quota. Use cx accounts list to inspect accounts.",
        )
}

async fn manage(root: &Path, accounts: &Accounts, overrides: &CliConfigOverrides) -> Result<()> {
    match &accounts.action {
        Some(
            Action::Add { name }
            | Action::Login { name }
            | Action::Logout { name }
            | Action::Remove { name },
        ) => {
            let remove = matches!(accounts.action, Some(Action::Remove { .. }));
            let add = matches!(accounts.action, Some(Action::Add { .. }));
            ensure!(
                name != "auto" && (!(add || remove) || name != "default"),
                "Choose a named account"
            );
            let profile = home(root, name);
            let marker = profile.join("cx-account-removed");
            if add {
                ensure!(
                    !names(root)?.contains(name),
                    "Account {name} already exists. Use accounts login {name} to sign in again"
                );
                create_home(&profile)?;
                fs::write(&marker, b"")?;
            } else if remove {
                ensure!(names(root)?.contains(name), "Unknown account {name}");
            }
            let action = if remove || matches!(accounts.action, Some(Action::Logout { .. })) {
                "logout"
            } else {
                "login"
            };
            let status = Command::new(std::env::current_exe()?)
                .args(
                    overrides
                        .raw_overrides
                        .iter()
                        .flat_map(|value| ["-c", value.as_str()]),
                )
                .arg(format!("--account={name}"))
                .arg(action)
                .status()?;
            ensure!(status.success(), "Account {action} failed");
            if action == "login" && marker.try_exists()? {
                fs::remove_file(&marker)?;
            }
            if remove {
                if read_selection(root)?.as_ref() == Some(name) {
                    select(root, "default")?;
                }
                fs::write(marker, b"")?;
                println!("Removed account {name}. Local history was retained.");
            }
            return Ok(());
        }
        _ => (),
    }
    if let Some(Action::Use { name }) = &accounts.action {
        return select(root, name);
    }
    let selected = read_selection(root)?.unwrap_or_else(|| "default".into());
    if matches!(accounts.action, Some(Action::Current)) {
        if selected == "auto" {
            println!("{}", choose(&statuses(root, overrides).await?)?);
        } else {
            println!("{selected}");
        }
        return Ok(());
    }
    let rows = statuses(root, overrides).await?;
    if matches!(accounts.action, Some(Action::List { json: true })) {
        serde_json::to_writer(
            io::stdout().lock(),
            &serde_json::json!({"selection":selected,"accounts":rows}),
        )?;
        println!();
        return Ok(());
    }
    println!("Selection: {selected}");
    for row in &rows {
        if let Some(q) = &row.quota {
            let remaining = if q.unlimited {
                "unlimited".into()
            } else {
                q.remaining_percent
                    .map_or_else(|| "unknown".into(), |v| format!("{v:.0}%"))
            };
            let resets = q
                .resets_at
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map_or_else(|| "unknown".into(), |d| d.to_rfc3339());
            println!(
                "{}  {}  {remaining} left  resets {resets}  reset credits {}{}",
                row.name,
                q.email
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect::<String>(),
                q.reset_credits
                    .map_or_else(|| "unknown".into(), |n| n.to_string()),
                if q.available { "" } else { "  unavailable" }
            );
        } else {
            println!("{}  quota unavailable", row.name);
        }
    }
    if accounts.action.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal() {
        print!(
            "Choose a name or auto, or enter add/remove/login/logout/use NAME (Enter to keep {selected}): "
        );
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] => (),
            [name] => select(root, &account_name(name).map_err(anyhow::Error::msg)?)?,
            [
                action @ ("add" | "remove" | "login" | "logout" | "use"),
                name,
            ] => {
                let name = account_name(name).map_err(anyhow::Error::msg)?;
                let status = Command::new(std::env::current_exe()?)
                    .args(
                        overrides
                            .raw_overrides
                            .iter()
                            .flat_map(|value| ["-c", value.as_str()]),
                    )
                    .args(["accounts", action, "--", &name])
                    .status()?;
                ensure!(status.success(), "Account {action} failed");
            }
            _ => anyhow::bail!("Invalid account action"),
        }
    }
    Ok(())
}
