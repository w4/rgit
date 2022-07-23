use std::collections::HashMap;

use comrak::adapters::SyntaxHighlighterAdapter;
use syntect::{
    html::{ClassStyle, ClassedHTMLGenerator},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

pub struct ComrakSyntectAdapter<'a> {
    pub(crate) syntax_set: &'a SyntaxSet,
}

impl SyntaxHighlighterAdapter for ComrakSyntectAdapter<'_> {
    fn highlight(&self, lang: Option<&str>, code: &str) -> String {
        let syntax = lang
            .and_then(|v| self.syntax_set.find_syntax_by_token(v))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, self.syntax_set, ClassStyle::Spaced);

        for line in LinesWithEndings::from(code) {
            let _res = html_generator.parse_html_for_line_which_includes_newline(line);
        }

        format!(
            "<code>{}</code>",
            html_generator.finalize().replace('\n', "</code>\n<code>")
        )
    }

    fn build_pre_tag(&self, _attributes: &HashMap<String, String>) -> String {
        r#"<pre>"#.to_string()
    }

    fn build_code_tag(&self, _attributes: &HashMap<String, String>) -> String {
        String::with_capacity(0)
    }
}
