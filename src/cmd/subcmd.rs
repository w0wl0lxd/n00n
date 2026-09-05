use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::{Context, bail};

use n00n_agent::mcp::{config as mcp_config, oauth as mcp_oauth};
use n00n_agent::tools::ToolRegistry;
use n00n_config::providers::{
    BuiltInProvider, Protocol, ProviderAccountDef, ProviderDef, ProvidersConfig, all_builtins,
    builtin_provider, resolve_api_key_env, resolve_base_url, resolve_default_model,
    resolve_display_name, resolve_login_url, slugify,
};
use n00n_config::{load_env_files, load_permissions};
use n00n_lua::PluginHost;
use n00n_providers::provider::fetch_all_models;
use n00n_providers::{ProviderData, catalog_providers, devin_legacy_account_name};
use n00n_providers::{copilot_auth, dynamic, openai_auth};
use n00n_storage::StateDir;
use n00n_storage::auth::ProviderCredentials;
use n00n_storage::auth::{
    delete_provider_credentials, load_provider_credentials, save_provider_credentials,
};
use n00n_storage::model::persist_model;

use crate::cli::SafetyFlags;

fn env_key_populated(var: &str) -> bool {
    env::var(var).is_ok_and(|v| v.split(',').any(|s| !s.trim().is_empty()))
}

fn legacy_devin_account<'a>(slug: &'a str, definition: Option<&ProviderDef>) -> Option<&'a str> {
    if !definition.is_some_and(|definition| definition.protocol == Some(Protocol::Devin)) {
        return None;
    }
    devin_legacy_account_name(slug)
}

fn resolved_devin_model_id(config: &ProvidersConfig) -> Result<String> {
    let model = resolve_default_model("devin", config.get("devin"))
        .ok_or_else(|| color_eyre::eyre::eyre!("no default model for Devin"))?;
    Ok(match model.strip_prefix("devin/") {
        Some(model_id) => model_id.to_string(),
        None => model,
    })
}

fn legacy_devin_slug_for_account(config: &ProvidersConfig, account: &str) -> Option<String> {
    let slug = format!("devin{account}");
    if legacy_devin_account(&slug, config.get(&slug)).is_some() {
        Some(slug)
    } else {
        None
    }
}

fn builtin_env_key(b: &BuiltInProvider) -> Option<&'static str> {
    env_key_populated(b.default_api_key_env).then_some(b.default_api_key_env)
}

pub fn auth_login(provider: Option<&str>, storage: &StateDir) -> Result<()> {
    match provider {
        Some("openai" | "codex") => openai_auth::login(storage)?,
        Some("copilot") => copilot_auth::login(storage)?,
        Some(selector) if selector.split_once('@').is_some() => {
            let (provider, account) = selector.split_once('@').ok_or_else(|| {
                color_eyre::eyre::eyre!("provider account must use provider@account")
            })?;
            login_provider_account(provider, account, storage)?;
        }
        Some(slug) => {
            let slug = slugify(slug);
            if builtin_provider(&slug).is_some()
                || dynamic::display_name(&slug).is_some()
                || ProvidersConfig::load_or_exit().get(&slug).is_some()
            {
                login_provider(&slug, storage)?;
            } else if let Some(provider_data) = n00n_providers::catalog_provider(&slug) {
                login_catalog_provider(&provider_data, storage)?;
            } else {
                login_custom(storage, Some(&slug))?;
            }
        }
        None => login_interactive(storage)?,
    }
    Ok(())
}

fn login_provider(slug: &str, storage: &StateDir) -> Result<()> {
    let builtin = builtin_provider(slug);
    let is_custom = ProvidersConfig::load_or_exit().get(slug).is_some();
    if builtin.is_none() && dynamic::display_name(slug).is_none() && !is_custom {
        bail!("unknown provider '{slug}'");
    }

    if builtin.is_none() && dynamic::auth_providers().iter().any(|(s, _)| *s == slug) {
        dynamic::login(slug)?;
        return Ok(());
    }

    let mut config = ProvidersConfig::load().context("read providers.toml")?;
    let def = config.get(slug).cloned();

    let plan = select_plan(slug, builtin, def.as_ref())?;

    let needs_url = builtin.is_some_and(|b| b.needs_url);
    let host_url = if needs_url {
        Some(prompt_host_url(
            slug,
            &resolve_display_name(slug, def.as_ref()),
            def.as_ref(),
        )?)
    } else {
        None
    };

    let api_key_optional = needs_url;
    let login_url = resolve_login_url(slug, plan.as_deref());
    let api_key = prompt_api_key(
        login_url.as_deref(),
        &resolve_display_name(slug, def.as_ref()),
        api_key_optional,
    )?;

    let mut provider_def = def.unwrap_or_else(Default::default);
    if let Some(plan_name) = &plan {
        provider_def.plan = Some(plan_name.clone());
    }
    if let Some(url) = &host_url {
        provider_def.base_url = Some(url.clone());
    }

    let has_key = !api_key.is_empty();
    if has_key {
        let creds = ProviderCredentials {
            api_key,
            host: None,
        };
        save_provider_credentials(storage, slug, &creds).context("save credentials")?;
    }

    if plan.is_some() || needs_url || host_url.is_some() || builtin.is_none() {
        config.upsert(slug.to_string(), provider_def);
        config.save().context("save providers.toml")?;
    }

    let default_model = if needs_url {
        None
    } else {
        resolve_default_model(slug, config.get(slug))
    };
    if let Some(model) = &default_model {
        persist_model(storage, model);
    }

    println!();
    let display = resolve_display_name(slug, config.get(slug));
    println!("  \x1b[32m✓\x1b[0m Configured: {display}");
    if let Some(url) = resolve_base_url(slug, config.get(slug)) {
        println!("  Endpoint: {url}");
    }
    if let Some(model) = &default_model {
        println!("  Default model: {model}");
    }
    if has_key {
        println!("  Credentials: ~/.local/state/n00n/auth/{slug}.json");
    } else {
        let env_var = resolve_api_key_env(slug, config.get(slug));
        println!("  Set API key via: {env_var} or run: n00n auth login {slug}");
    }

    Ok(())
}

fn validated_provider_account(account: &str) -> Result<String> {
    let normalized = slugify(account);
    if account.is_empty() || account != normalized {
        bail!("provider account must already be a lowercase slug");
    }
    Ok(normalized)
}

fn configure_provider_account(definition: &mut ProviderDef, account: &str, has_api_key: bool) {
    let account_definition = definition
        .accounts
        .entry(account.to_string())
        .or_insert_with(ProviderAccountDef::default);
    if has_api_key {
        account_definition.credential_path = None;
    }
}

fn login_provider_account(provider: &str, account: &str, storage: &StateDir) -> Result<()> {
    let provider = slugify(provider);
    let account = validated_provider_account(account)?;
    if provider != "devin" {
        bail!("provider accounts are currently supported for Devin");
    }

    let selector = format!("{provider}@{account}");
    let mut config = ProvidersConfig::load().context("read providers.toml")?;
    let api_key = prompt_api_key(None, &format!("Devin account {account}"), true)?;
    let has_api_key = !api_key.is_empty();
    if has_api_key {
        save_provider_credentials(
            storage,
            &selector,
            &ProviderCredentials {
                api_key,
                host: None,
            },
        )
        .context("save account credentials")?;
    } else if !n00n_providers::devin_account_has_credentials(&account) {
        bail!(
            "Devin account '{account}' has no usable stored, configured, legacy, or CLI credentials"
        );
    }

    let mut definition = config
        .get(&provider)
        .cloned()
        .unwrap_or_else(Default::default);
    configure_provider_account(&mut definition, &account, has_api_key);
    config.upsert(provider, definition);
    config.save().context("save providers.toml")?;

    let model_id = resolved_devin_model_id(&config)?;
    let model = format!("devin/{account}::{model_id}");
    persist_model(storage, &model);
    println!("  \x1b[32m✓\x1b[0m Configured: Devin (account {account})");
    println!("  Default model: {model}");
    if has_api_key {
        println!("  Credentials: ~/.local/state/n00n/auth/{selector}.json");
    } else {
        println!(
            "  Using configured credential_path or the standard ~/.local/share/devin{account}/devin/credentials.toml path"
        );
    }
    Ok(())
}

fn login_interactive(storage: &StateDir) -> Result<()> {
    let builtins = all_builtins();
    let config = ProvidersConfig::load_or_exit();
    let custom_slugs: Vec<&String> = config
        .providers
        .keys()
        .filter(|s| {
            builtin_provider(s).is_none()
                && *s != "opencode"
                && legacy_devin_account(s, config.get(s)).is_none()
        })
        .collect();

    println!();
    println!("  Available providers:");
    println!();
    for (i, b) in builtins.iter().enumerate() {
        let status = if load_provider_credentials(storage, b.slug).is_some() {
            "\x1b[32m✓\x1b[0m"
        } else if builtin_env_key(b).is_some() {
            "\x1b[33m~\x1b[0m"
        } else {
            " "
        };
        println!("  {} {}. {:<14} {}", status, i + 1, b.slug, b.display_name);
    }
    let mut idx = builtins.len();
    for slug in &custom_slugs {
        idx += 1;
        let status = if load_provider_credentials(storage, slug).is_some() {
            "\x1b[32m✓\x1b[0m"
        } else {
            " "
        };
        let display = config
            .get(slug)
            .and_then(|d| d.display_name.as_deref())
            .unwrap_or_else(|| slug);
        println!("  {status} {idx}. {slug:<14} {display}");
    }

    let catalog_entries = catalog_providers();
    for cat in &catalog_entries {
        idx += 1;
        let status = if load_provider_credentials(storage, &cat.slug).is_some() {
            "\x1b[32m✓\x1b[0m"
        } else {
            " "
        };
        println!(
            "  {} {}. {:<14} {}",
            status, idx, cat.slug, cat.display_name
        );
    }
    idx += 1;
    let custom_idx = idx;
    println!("    {custom_idx}. Custom provider...");
    println!();

    print!("  Select [1-{custom_idx}]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice: usize = input.trim().parse().context("enter a number")?;

    if choice == 0 || choice > custom_idx {
        bail!("invalid selection");
    }

    if choice == custom_idx {
        login_custom(storage, None)?;
    } else if choice <= builtins.len() {
        let slug = builtins[choice - 1].slug;
        login_provider(slug, storage)?;
    } else if choice <= builtins.len() + custom_slugs.len() {
        let slug = custom_slugs[choice - builtins.len() - 1];
        login_provider(slug, storage)?;
    } else {
        let provider = &catalog_entries[choice - builtins.len() - custom_slugs.len() - 1];
        login_catalog_provider(provider, storage)?;
    }

    Ok(())
}

fn login_catalog_provider(provider: &ProviderData, storage: &StateDir) -> Result<()> {
    println!();
    if let Some(var) = provider.env_keys.first() {
        println!("  Provider: {} (env: {var})", provider.slug);
    } else {
        println!("  Provider: {}", provider.slug);
    }
    print!("  API key: ");
    io::stdout().flush()?;
    let mut key = String::new();
    io::stdin().read_line(&mut key)?;
    let key = key.trim().to_string();
    if key.is_empty() {
        println!("  Skipped (no key entered)");
        return Ok(());
    }
    let creds = ProviderCredentials {
        api_key: key,
        host: None,
    };
    save_provider_credentials(storage, &provider.slug, &creds).context("save credentials")?;
    println!("  \x1b[32m✓\x1b[0m Saved credentials for {}", provider.slug);
    println!(
        "  Credentials: ~/.local/state/n00n/auth/{}.json",
        provider.slug
    );
    println!(
        "  You can also set via: {}",
        provider
            .env_keys
            .first()
            .cloned()
            .unwrap_or_else(|| "API key environment variable".to_string())
    );
    Ok(())
}

fn login_custom(storage: &StateDir, slug: Option<&str>) -> Result<()> {
    let slug = if let Some(s) = slug {
        s.to_string()
    } else {
        print!("  Provider name: ");
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().read_line(&mut name)?;
        let slug = slugify(&name);
        if slug.is_empty() {
            bail!("provider name cannot be empty");
        }
        slug
    };

    println!("  Protocol:");
    println!("    1. openai   (OpenAI-compatible chat completions)");
    println!("    2. anthropic (Anthropic messages API)");
    println!("    3. google   (Google Gemini API)");
    println!("    4. devin    (Devin ACP via devin acp subprocess)");
    print!("  Select [1-4]: ");
    io::stdout().flush()?;
    let mut proto_input = String::new();
    io::stdin().read_line(&mut proto_input)?;
    let protocol = match proto_input.trim() {
        "1" | "openai" => "openai",
        "2" | "anthropic" => "anthropic",
        "3" | "google" => "google",
        "4" | "devin" => "devin",
        _ => {
            bail!("invalid protocol selection");
        }
    };

    let needs_url = protocol != "devin";
    print!(
        "  Base URL{}: ",
        if needs_url { "" } else { " (or Enter to skip)" }
    );
    io::stdout().flush()?;
    let mut url_input = String::new();
    io::stdin().read_line(&mut url_input)?;
    let base_url = url_input.trim().to_string();
    if needs_url && base_url.is_empty() {
        bail!("base URL cannot be empty");
    }

    let display_name = format!("Custom ({slug})");
    let api_key_env = format!("{}_API_KEY", slug.to_uppercase().replace('-', "_"));

    print!("  API key (or Enter to skip): ");
    io::stdout().flush()?;
    let mut key_input = String::new();
    io::stdin().read_line(&mut key_input)?;
    let api_key = key_input.trim().to_string();

    let mut config = ProvidersConfig::load().context("read providers.toml")?;
    let default_model = if protocol == "devin" {
        Some(format!("{slug}/{}", resolved_devin_model_id(&config)?))
    } else {
        None
    };
    let provider_def = ProviderDef {
        display_name: Some(display_name),
        protocol: Some(
            protocol
                .parse()
                .map_err(|e: String| color_eyre::eyre::eyre!("{e}"))?,
        ),
        base_url: if base_url.is_empty() {
            None
        } else {
            Some(base_url.clone())
        },
        api_key_env: Some(api_key_env.clone()),
        default_model,
        discover_models: true,
        ..Default::default()
    };

    let has_key = !api_key.is_empty();
    if has_key {
        let creds = ProviderCredentials {
            api_key,
            host: None,
        };
        save_provider_credentials(storage, &slug, &creds).context("save credentials")?;
    }

    config.upsert(slug.clone(), provider_def);
    config.save().context("save providers.toml")?;

    println!();
    println!("  \x1b[32m✓\x1b[0m Configured: {slug}");
    if !base_url.is_empty() {
        println!("  Endpoint: {base_url}");
    }
    if let Some(model) = config.get(&slug).and_then(|d| d.default_model.as_deref()) {
        println!("  Default model: {model}");
    }
    if has_key {
        println!("  Credentials: ~/.local/state/n00n/auth/{slug}.json");
    } else {
        println!("  Set API key via: {api_key_env} or run: n00n auth login {slug}");
    }
    println!("  Use with: n00n -m {slug}/<model>");

    Ok(())
}

fn select_plan(
    slug: &str,
    builtin: Option<&'static n00n_config::providers::BuiltInProvider>,
    def: Option<&ProviderDef>,
) -> Result<Option<String>> {
    let plans = builtin.and_then(|b| b.plans);
    if plans.is_none_or(|p| p.len() <= 1) {
        if let Some(d) = def {
            return Ok(d.plan.clone());
        }
        return Ok(None);
    }
    let Some(plans) = plans else { return Ok(None) };

    if let Some(d) = def
        && d.plan.is_some()
    {
        return Ok(d.plan.clone());
    }

    println!();
    println!("  {} plan:", resolve_display_name(slug, def));
    for (i, (_key, plan)) in plans.iter().enumerate() {
        println!("    {}. {} ({})", i + 1, plan.display_name, plan.base_url);
    }
    println!();
    print!("  Select [1-{}]: ", plans.len());
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice: usize = input.trim().parse().context("enter a number")?;
    if choice == 0 || choice > plans.len() {
        bail!("invalid plan selection");
    }
    Ok(Some(plans[choice - 1].0.to_string()))
}

fn prompt_host_url(slug: &str, display_name: &str, def: Option<&ProviderDef>) -> Result<String> {
    let default = resolve_base_url(slug, def).unwrap_or_else(String::new);
    print!("  {display_name} host URL [{default}]: ");
    io::stdout().flush()?;

    let mut url = String::new();
    io::stdin().read_line(&mut url)?;
    let url = url.trim().to_string();

    Ok(if url.is_empty() { default } else { url })
}

fn prompt_api_key(url: Option<&str>, display_name: &str, optional: bool) -> Result<String> {
    if let Some(url) = url {
        if let Err(e) = open::that(url) {
            tracing::warn!(error = %e, "failed to open browser");
        }
        println!("  Opened {url} in your browser.");
    }
    if optional {
        print!("  {display_name} API key (or Enter to skip): ");
    } else {
        print!("  {display_name} API key: ");
    }
    io::stdout().flush()?;

    let mut api_key = String::new();
    io::stdin().read_line(&mut api_key)?;
    let api_key = api_key.trim().to_string();

    Ok(api_key)
}

pub fn auth_logout(provider: &str, storage: &StateDir, safety: SafetyFlags) -> Result<()> {
    if !crate::safety::allow(
        safety,
        &format!("remove stored credentials for '{provider}'"),
    )? {
        return Ok(());
    }

    if let Some((provider, account)) = provider.split_once('@') {
        let provider = slugify(provider);
        let account = validated_provider_account(account)?;
        if provider != "devin" {
            bail!("provider accounts are currently supported for Devin");
        }
        let selector = format!("{provider}@{account}");
        let deleted =
            delete_provider_credentials(storage, &selector).context("delete credentials")?;
        let mut config = ProvidersConfig::load().context("read providers.toml")?;
        let legacy_slug = legacy_devin_slug_for_account(&config, &account);
        let legacy_deleted = match legacy_slug.as_deref() {
            Some(slug) => {
                delete_provider_credentials(storage, slug).context("delete legacy credentials")?
            }
            None => false,
        };
        let removed = config
            .providers
            .get_mut(&provider)
            .is_some_and(|definition| definition.accounts.remove(&account).is_some());
        let legacy_removed = legacy_slug.is_some_and(|slug| config.remove(&slug));
        if removed || legacy_removed {
            config.save().context("save providers.toml")?;
        }
        if deleted || legacy_deleted || removed || legacy_removed {
            println!("Removed credentials for '{selector}'.");
        } else {
            println!("No stored credentials for '{selector}'.");
        }
        return Ok(());
    }

    let slug = slugify(provider);
    match slug.as_str() {
        "openai" | "codex" => openai_auth::logout(storage)?,
        "copilot" => copilot_auth::logout(storage)?,
        _ => {
            let mut config = ProvidersConfig::load().context("read providers.toml")?;
            let deleted =
                delete_provider_credentials(storage, &slug).context("delete credentials")?;
            if deleted {
                println!("Removed credentials for '{slug}'.");
            }
            if config.remove(&slug) {
                config.save().context("save providers.toml")?;
            }
            if !deleted && builtin_provider(&slug).is_none() {
                dynamic::logout(&slug)?;
            }
        }
    }
    Ok(())
}

pub fn auth_status(storage: &StateDir) {
    let config = ProvidersConfig::load_or_exit();
    let builtins = all_builtins();

    println!();
    for b in &builtins {
        let def = config.get(b.slug);
        let display = resolve_display_name(b.slug, def);

        if b.slug == "codex" {
            if openai_auth::is_oauth(storage) {
                println!("  \x1b[32m✓\x1b[0m {:<14} {} (OAuth)", b.slug, display);
            } else {
                println!(
                    "  \x1b[31m✗\x1b[0m {:<14} {} (run: n00n auth login {})",
                    b.slug, display, b.slug
                );
            }
            continue;
        }

        if b.slug == "devin" {
            if n00n_providers::devin_primary_has_credentials() {
                println!("  \x1b[32m✓\x1b[0m {:<14} {}", b.slug, display);
            } else {
                println!(
                    "  \x1b[31m✗\x1b[0m {:<14} {} (run: n00n auth login {})",
                    b.slug, display, b.slug
                );
            }
            continue;
        }

        if let Some(creds) = load_provider_credentials(storage, b.slug) {
            let plan_info = def
                .and_then(|d| d.plan.as_deref())
                .map_or_else(String::new, |p| format!(" ({p})"));
            let masked = if creds.api_key.len() > 8 {
                format!(
                    "{}...{}",
                    &creds.api_key[..4],
                    &creds.api_key[creds.api_key.len() - 4..]
                )
            } else {
                "****".to_string()
            };
            println!(
                "  \x1b[32m✓\x1b[0m {:<14} {} (key: {}){}",
                b.slug, display, masked, plan_info
            );
        } else if let Some(env_key) = builtin_env_key(b) {
            println!(
                "  \x1b[33m~\x1b[0m {:<14} {} (via {})",
                b.slug, display, env_key
            );
        } else if def.is_some_and(|d| d.base_url.is_some()) {
            println!("  \x1b[34m●\x1b[0m {:<14} {} (configured)", b.slug, display);
        } else {
            println!(
                "  \x1b[31m✗\x1b[0m {:<14} {} (run: n00n auth login {})",
                b.slug, display, b.slug
            );
        }
    }

    for account in n00n_providers::devin_account_names(&config) {
        let status = if n00n_providers::devin_account_has_credentials(&account) {
            "\x1b[32m✓\x1b[0m"
        } else {
            "\x1b[31m✗\x1b[0m"
        };
        let display = config
            .get("devin")
            .and_then(|definition| definition.accounts.get(&account))
            .and_then(|definition| definition.display_name.as_deref())
            .map_or_else(|| format!("Devin (account {account})"), str::to_string);
        println!("  {status} {:<14} {display}", format!("devin@{account}"));
    }

    for (slug, def) in &config.providers {
        // 'opencode' could show up here, when the user configured free models on that provider.
        if builtin_provider(slug).is_some()
            || legacy_devin_account(slug, Some(def)).is_some()
            || (slug == "opencode" && def.enable_free_models.is_some())
        {
            continue;
        }
        let display = def.display_name.as_deref().unwrap_or_else(|| slug);
        if let Some(creds) = load_provider_credentials(storage, slug) {
            println!(
                "  \x1b[32m✓\x1b[0m {:<14} {} (key: {})",
                slug,
                display,
                creds.masked_api_key()
            );
        } else {
            let default_env = format!("{}_API_KEY", slug.to_uppercase().replace('-', "_"));
            let env_var = def.api_key_env.as_deref().unwrap_or_else(|| &default_env);
            if env::var(env_var).is_ok() {
                println!("  \x1b[33m~\x1b[0m {slug:<14} {display} (via {env_var})");
            } else {
                println!("  \x1b[31m✗\x1b[0m {slug:<14} {display} (run: n00n auth login {slug})");
            }
        }
    }
    // Catalog providers from models.dev
    let catalog_entries = catalog_providers();
    if !catalog_entries.is_empty() {
        println!("  \x1b[1mCatalog Providers (models.dev):\x1b[0m");
        for entry in &catalog_entries {
            if let Some(creds) = load_provider_credentials(storage, &entry.slug) {
                println!(
                    "  \x1b[32m✓\x1b[0m {:<14} {} (key: {})",
                    entry.slug,
                    entry.display_name,
                    creds.masked_api_key()
                );
            } else if let Some(env) = entry.env_key_set() {
                println!(
                    "  \x1b[33m~\x1b[0m {:<14} {} (via {})",
                    entry.slug, entry.display_name, env
                );
            } else {
                println!(
                    "  \x1b[31m✗\x1b[0m {:<14} {} (run: n00n auth login {})",
                    entry.slug, entry.display_name, entry.slug
                );
            }
        }
        println!();
    }
}

pub fn models() {
    smol::block_on(fetch_all_models(
        |batch| {
            for model in batch.models {
                println!("{model}");
            }
            for warning in batch.warnings {
                eprintln!("warning: {warning}");
            }
        },
        None,
    ));
}

pub fn index(path: &str, no_plugins: bool, no_jit: bool, project_trusted: bool) -> Result<()> {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    load_env_files(&cwd);

    let mut host = if no_plugins {
        PluginHost::disabled()
    } else {
        PluginHost::with_jit(Arc::clone(ToolRegistry::global_arc()), !no_jit)
            .context("initialize lua plugin host")?
    };

    let raw_config = host
        .load_init_files(&cwd, project_trusted)
        .context("load init.lua files")?;

    let mut config = raw_config
        .unwrap_or_else(Default::default)
        .into_config(false)
        .context("invalid config")?;
    config.permissions = load_permissions(&cwd, project_trusted);
    config.project_trusted = project_trusted;

    host.set_search_config(Arc::new(config.search.clone()))
        .context("configure lua search services")?;
    host.load_builtins(&config.plugins)
        .context("load builtin plugins")?;

    let abs_path = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf());
    let input = serde_json::json!({"path": abs_path.to_str().unwrap_or_else(|| path)});
    let reg = ToolRegistry::global_arc();
    let entry = reg
        .get("index")
        .ok_or_else(|| color_eyre::eyre::eyre!("index tool not registered"))?;
    let inv = entry
        .tool
        .parse(&input)
        .map_err(|e| color_eyre::eyre::eyre!("parse index input: {e}"))?;
    let ctx = n00n_agent::tools::cli_tool_ctx();
    let result = smol::block_on(async { inv.execute(&ctx).await });
    match result.output {
        Ok(output) => print!("{}", output.as_text()),
        Err(e) => {
            bail!("index failed: {e}");
        }
    }
    Ok(())
}

pub fn mcp_auth(server: &str, storage: &StateDir, project_trusted: bool) -> Result<()> {
    smol::block_on(async {
        let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
        let (config, _) = mcp_config::load_config(&cwd, project_trusted);
        let raw = config
            .mcp
            .get(server)
            .ok_or_else(|| color_eyre::eyre::eyre!("unknown MCP server: {server}"))?;
        let mcp_config::Transport::Http { url, .. } =
            mcp_config::parse_server(server.to_owned(), raw.clone())?.transport
        else {
            color_eyre::eyre::bail!("server '{server}' is not an HTTP transport");
        };
        mcp_oauth::authenticate(server, &url, None, storage, mcp_oauth::Interaction::Cli).await?;
        eprintln!("Successfully authenticated with MCP server '{server}'");
        Ok(())
    })
}

pub fn mcp_logout(server: &str, storage: &StateDir, safety: SafetyFlags) -> Result<()> {
    if !crate::safety::allow(
        safety,
        &format!("remove stored OAuth credentials for MCP server '{server}'"),
    )? {
        return Ok(());
    }

    let deleted = n00n_storage::auth::delete_mcp_auth(storage, server)?;
    if deleted {
        eprintln!("Removed OAuth credentials for MCP server '{server}'");
    } else {
        eprintln!("No stored credentials for MCP server '{server}'");
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub struct PromptFlags {
    pub plan: bool,
    pub tools: bool,
    pub names: bool,
    pub no_jit: bool,
    pub project_trusted: bool,
}

pub fn prompt(variant: &crate::cli::PromptVariant, flags: PromptFlags) -> Result<()> {
    use crate::cli::PromptVariant;
    use n00n_agent::agent::{build_system_prompt, load_instruction_text};
    use n00n_agent::prompt::{PromptId, assemble};
    use n00n_agent::template;
    use n00n_agent::tools::{DescriptionContext, ToolAudience, ToolFilter, ToolRegistry};
    use n00n_providers::Model;

    if flags.plan && !matches!(variant, PromptVariant::System) {
        bail!("--plan can only be used with the 'system' prompt variant");
    }

    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    load_env_files(&cwd);

    let vars = template::env_vars();
    let reg = ToolRegistry::global_arc();
    let mut host = PluginHost::with_jit(Arc::clone(reg), !flags.no_jit)
        .context("initialize lua plugin host")?;
    let raw_config = host
        .load_init_files(&cwd, flags.project_trusted)
        .context("load init.lua files")?;
    let config = raw_config
        .unwrap_or_else(Default::default)
        .into_config(false)
        .context("invalid config")?;

    host.set_search_config(Arc::new(config.search.clone()))
        .context("configure lua search services")?;
    host.load_builtins(&config.plugins)
        .context("load builtin plugins")?;

    if flags.tools {
        let ctx = DescriptionContext {
            filter: &ToolFilter::All,
            audience: ToolAudience::MAIN,
            workflow: false,
        };
        let defs = reg.definitions(&vars, &ctx, true);
        if flags.names {
            for name in defs
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d["name"].as_str())
            {
                println!("{name}");
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&defs)?);
        }
        return Ok(());
    }

    let cwd_str = cwd.to_string_lossy();
    let instructions = load_instruction_text(&cwd_str);
    let slots = host
        .event_handle()
        .map_or_else(Default::default, |h| h.collect_prompt_slots());

    let output = match variant {
        PromptVariant::System => {
            let mode = if flags.plan {
                n00n_agent::AgentMode::Plan(std::path::PathBuf::from("plan.md"))
            } else {
                n00n_agent::AgentMode::Build
            };
            let model_spec = config
                .provider
                .default_model
                .as_deref()
                .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514");
            let model = Model::from_spec(model_spec).context("invalid default model")?;
            build_system_prompt(&vars, &mode, &instructions, &slots, &model)
        }
        PromptVariant::Research => assemble(PromptId::Research, &slots, &instructions).into(),
        PromptVariant::General => assemble(PromptId::General, &slots, &instructions).into(),
    };

    print!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devin_default_model_resolution_preserves_nested_ids() {
        let mut config = ProvidersConfig::default();
        assert_eq!(
            resolved_devin_model_id(&config).expect("builtin default model"),
            "swe-1-7"
        );
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                default_model: Some("devin/org/custom-model".to_string()),
                ..ProviderDef::default()
            },
        );
        assert_eq!(
            resolved_devin_model_id(&config).expect("configured default model"),
            "org/custom-model"
        );
    }

    #[test]
    fn provider_account_selectors_must_already_be_lowercase_slugs() {
        assert_eq!(
            validated_provider_account("team-prod").expect("valid account"),
            "team-prod"
        );
        for invalid in ["", "Work", "work!", "work team"] {
            assert!(validated_provider_account(invalid).is_err());
        }
    }

    #[test]
    fn api_key_login_clears_authoritative_credential_path() {
        let mut definition = ProviderDef {
            accounts: std::collections::HashMap::from([(
                "work".to_string(),
                ProviderAccountDef {
                    display_name: Some("Work".to_string()),

                    credential_path: Some("credentials.toml".into()),
                },
            )]),
            ..ProviderDef::default()
        };

        configure_provider_account(&mut definition, "work", true);
        let account = definition.accounts.get("work").expect("account exists");
        assert_eq!(account.display_name.as_deref(), Some("Work"));
        assert_eq!(account.credential_path, None);
    }

    #[test]
    fn credential_file_login_preserves_authoritative_path() {
        let mut definition = ProviderDef {
            accounts: std::collections::HashMap::from([(
                "work".to_string(),
                ProviderAccountDef {
                    display_name: None,
                    credential_path: Some("credentials.toml".into()),
                },
            )]),
            ..ProviderDef::default()
        };

        configure_provider_account(&mut definition, "work", false);
        assert_eq!(
            definition
                .accounts
                .get("work")
                .and_then(|account| account.credential_path.as_deref()),
            Some(Path::new("credentials.toml"))
        );
    }

    #[test]
    fn legacy_devin_logout_only_targets_devin_protocol_aliases() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin2".to_string(),
            ProviderDef {
                protocol: Some(Protocol::Openai),
                ..ProviderDef::default()
            },
        );
        assert_eq!(legacy_devin_slug_for_account(&config, "2"), None);

        config
            .providers
            .get_mut("devin2")
            .expect("provider exists")
            .protocol = Some(Protocol::Devin);
        assert_eq!(
            legacy_devin_slug_for_account(&config, "2"),
            Some("devin2".to_string())
        );
        assert_eq!(legacy_devin_slug_for_account(&config, "work"), None);
    }
}
