use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use n00n_config::SearchConfig;
use n00n_search::{
    DEFAULT_MAX_REDIRECTS, ExtractRequest, Extractor, FetchLimits, HttpTransport, MAX_SOURCE_BYTES,
    UrlPolicy,
};
use std::sync::Arc;

use super::util::{convert::err_pair, ctx::LuaCtx};
use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};
use crate::plugin_permissions::{Permission, PluginPermissions};

pub(crate) fn create_search_table(
    lua: &Lua,
    permissions: &PluginPermissions,
    search_config: Arc<SearchConfig>,
) -> LuaResult<Table> {
    let table = lua.create_table()?;
    let permissions = permissions.clone();
    table.set(
        "extract",
        lua.create_async_function(
            move |lua, (ctx, request): (mlua::UserDataRef<LuaCtx>, Value)| {
                let permissions = permissions.clone();
                let search_config = search_config.clone();
                async move { extract(lua, ctx, permissions, search_config, request).await }
            },
        )?,
    )?;
    Ok(table)
}

async fn extract(
    lua: Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    permissions: PluginPermissions,
    search_config: Arc<SearchConfig>,
    request: Value,
) -> LuaResult<(Value, Value)> {
    if let Err(error) = ctx.ensure_active() {
        return err_pair(&lua, error);
    }
    if !permissions.is_allowed(Permission::Net) {
        return err_pair(&lua, "n00n.search.extract requires the net permission");
    }
    if !search_config.enabled() {
        return err_pair(&lua, "n00n.search.extract is disabled by the search config");
    }
    let request = match lua.from_value::<ExtractRequest>(request) {
        Ok(request) => request,
        Err(error) => return err_pair(&lua, error),
    };
    let cancel = ctx.cancel_token();
    drop(ctx);
    let extractor = match Extractor::new(
        HttpTransport,
        UrlPolicy::untrusted_page(),
        FetchLimits {
            max_response_bytes: MAX_SOURCE_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        },
    ) {
        Ok(extractor) => extractor,
        Err(error) => return err_pair(&lua, error),
    };
    let response = match cancel.race(extractor.extract(&request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return err_pair(&lua, error),
        Err(error) => return err_pair(&lua, error),
    };
    Ok((lua.to_value(&response)?, Value::Nil))
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.search",
    kind: DocKind::Table,
    desc: "Native, keyless extraction of bounded public web content.",
    fns: &[FnDoc {
        name: "extract",
        args: "ctx, request",
        desc: "Extract public URLs with manual redirect validation, DNS pinning, byte limits, and tool cancellation. Requires [search].enabled = true and the plugin's net permission.",
        params: &[
            ParamDoc {
                name: "ctx",
                ty: "LuaCtx",
                desc: "Current tool context; cancellation stops the operation.",
            },
            ParamDoc {
                name: "request",
                ty: "table",
                desc: "Extraction request. Fields: urls (array of 1 to 20 public http(s) URLs), format (\"markdown\", \"text\", or \"html\"), max_bytes_per_source (1 to 10485760 bytes).",
            },
        ],
        returns: "(table|nil, string|nil) Extraction response or an error.",
        example: r#"local result, err = n00n.search.extract(ctx, {
  urls = { "https://example.com/doc" },
  format = "markdown",
  max_bytes_per_source = 262144,
})"#,
    }],
};

#[cfg(test)]
mod tests {
    use mlua::{Function, LuaSerdeExt};
    use n00n_agent::cancel::CancelToken;
    use n00n_config::{RawConfig, SearchFileConfig};
    use n00n_search::{ExtractFormat, ExtractRequest};

    use super::*;

    fn enabled_search_config() -> Arc<SearchConfig> {
        Arc::new(
            RawConfig {
                search: SearchFileConfig {
                    enabled: Some(true),
                },
                ..RawConfig::default()
            }
            .into_config(false)
            .unwrap()
            .search,
        )
    }

    fn request() -> ExtractRequest {
        ExtractRequest {
            urls: vec!["https://example.com".to_owned()],
            format: ExtractFormat::Text,
            max_bytes_per_source: 1_024,
        }
    }

    #[test]
    fn bridge_observes_context_cancellation_before_network_dispatch() {
        smol::block_on(async {
            let lua = Lua::new();
            let table =
                create_search_table(&lua, &PluginPermissions::trusted(), enabled_search_config())
                    .unwrap();
            let function: Function = table.get("extract").unwrap();
            let (trigger, token) = CancelToken::new();
            trigger.cancel();
            let ctx = lua.create_userdata(LuaCtx::for_test(token)).unwrap();
            let input = lua.to_value(&request()).unwrap();
            let (value, error): (Value, Value) = function.call_async((ctx, input)).await.unwrap();
            assert!(value.is_nil());
            assert_eq!(error.as_string().unwrap().to_str().unwrap(), "cancelled");
        });
    }

    #[test]
    fn bridge_returns_validation_errors_as_lua_pairs() {
        smol::block_on(async {
            let lua = Lua::new();
            let table =
                create_search_table(&lua, &PluginPermissions::trusted(), enabled_search_config())
                    .unwrap();
            let function: Function = table.get("extract").unwrap();
            let ctx = lua
                .create_userdata(LuaCtx::for_test(CancelToken::none()))
                .unwrap();
            let mut invalid = request();
            invalid.urls.clear();
            let input = lua.to_value(&invalid).unwrap();
            let (value, error): (Value, Value) = function.call_async((ctx, input)).await.unwrap();
            assert!(value.is_nil());
            assert!(
                error
                    .as_string()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("invalid urls")
            );
        });
    }
}
