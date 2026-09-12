use n00n_lua::docs_render;

const FRONTMATTER: &str = r#"+++
title = "Lua API"
weight = 6
[extra]
group = "Reference"
+++

"#;

pub fn generate() -> String {
    format!(
        "{FRONTMATTER}{{% raw %}}\n{}\n{{% endraw %}}\n",
        docs_render::site_page()
    )
}
