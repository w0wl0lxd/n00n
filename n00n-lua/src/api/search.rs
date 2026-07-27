use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use n00n_config::SearchConfig;
use n00n_search::{
    extract::Extractor,
    transport::{DEFAULT_MAX_REDIRECTS, FetchLimits, HttpTransport},
    types::{ExtractRequest, MAX_SOURCE_BYTES},
    url_policy::UrlPolicy,
};

use super::util::{convert::err_pair, ctx::LuaCtx};
use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

pub(crate) fn create_search_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;
    table.set(
        "extract",
        lua.create_async_function(
            |lua, (ctx, request): (mlua::UserDataRef<LuaCtx>, Value)| async move {
                extract(lua, ctx, request).await
            },
        )?,
    )?;
    Ok(table)
}

async fn extract(
    lua: Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    request: Value,
) -> LuaResult<(Value, Value)> {
    let request = match lua.from_value::<ExtractRequest>(request) {
        Ok(request) => request,
        Err(error) => return err_pair(&lua, error),
    };
    let enabled = lua
        .app_data_ref::<std::sync::Arc<SearchConfig>>()
        .is_some_and(|config| config.enabled());
    if !enabled {
        return err_pair(&lua, "native search is disabled");
    }
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
        desc: "Extract public URLs with manual redirect validation, DNS pinning, byte limits, and tool cancellation.",
        params: &[
            ParamDoc {
                name: "ctx",
                ty: "LuaCtx",
                desc: "Current tool context; cancellation stops the operation.",
            },
            ParamDoc {
                name: "request",
                ty: "table",
                desc: "Extraction request containing urls, format, query, chunks_per_source, and max_bytes_per_source.",
            },
        ],
        returns: "(table|nil, string|nil) Extraction response or an error.",
        example: "local result, err = n00n.search.extract(ctx, request)",
    }],
};

#[cfg(test)]
mod tests {
    use mlua::{Function, LuaSerdeExt};
    use n00n_agent::cancel::CancelToken;
    use n00n_config::{RawConfig, SearchFileConfig};
    use n00n_search::types::{ExtractFormat, ExtractRequest};

    use super::*;

    fn request() -> ExtractRequest {
        ExtractRequest {
            urls: vec!["https://example.com".to_owned()],
            format: ExtractFormat::Text,
            query: None,
            chunks_per_source: 0,
            max_bytes_per_source: 1_024,
        }
    }

    fn enabled_config() -> std::sync::Arc<SearchConfig> {
        std::sync::Arc::new(
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

    #[test]
    fn bridge_rejects_requests_when_native_search_is_disabled() {
        smol::block_on(async {
            let lua = Lua::new();
            lua.set_app_data(std::sync::Arc::new(SearchConfig::default()));
            let table = create_search_table(&lua).unwrap();
            let function: Function = table.get("extract").unwrap();
            let ctx = lua
                .create_userdata(LuaCtx::for_test(CancelToken::none()))
                .unwrap();
            let input = lua.to_value(&request()).unwrap();
            let (value, error): (Value, Value) = function.call_async((ctx, input)).await.unwrap();
            assert!(value.is_nil());
            assert_eq!(
                error.as_string().unwrap().to_str().unwrap(),
                "native search is disabled"
            );
        });
    }

    #[test]
    fn bridge_observes_context_cancellation_before_network_dispatch() {
        smol::block_on(async {
            let lua = Lua::new();
            lua.set_app_data(enabled_config());
            let table = create_search_table(&lua).unwrap();
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
            lua.set_app_data(enabled_config());
            let table = create_search_table(&lua).unwrap();
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
