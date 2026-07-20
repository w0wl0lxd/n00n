use mlua::{Lua, Result as LuaResult, Value};
use n00n_lua_macro::{lua_fn, lua_table};

/// Convert an HTML string to Markdown.
/// Useful for cleaning up web content fetched with `n00n.webfetch`.
///
/// @param html string HTML source text.
/// @return (string?, string?) Markdown text on success, or nil plus an error message.
/// @example
/// local md, err = n00n.text.html_to_markdown("<h1>Hello</h1><p>world</p>")
/// if err then return end
/// print(md) -- "# Hello\n\nworld"
#[lua_fn]
fn html_to_markdown(lua: &Lua, html: String) -> LuaResult<(Value, Value)> {
    match htmd::convert(&html) {
        Ok(md) => Ok((Value::String(lua.create_string(&md)?), Value::Nil)),
        Err(e) => Ok((
            Value::Nil,
            Value::String(lua.create_string(format!("html_to_markdown: {e}"))?),
        )),
    }
}

lua_table! {
    /// Text transformation utilities.
    ///
    /// Helper functions for converting between text formats.
    ///
    /// ```lua
    /// local md = n00n.text.html_to_markdown(html)
    /// ```
    "n00n.text" => pub(crate) fn create_text_table(), DOCS [
        html_to_markdown,
    ]
}
